import argparse
import json
from pathlib import Path
import re
import sys
import time
from functools import lru_cache


def _require_transformers():
    try:
        import torch  # noqa: F401
        from huggingface_hub import snapshot_download  # noqa: F401
        from transformers import AutoModelForCausalLM, AutoTokenizer  # noqa: F401
    except ImportError as error:
        raise SystemExit(
            "Qwen3 receipt cleanup requires Python packages `transformers`, `huggingface_hub`, and `safetensors`. "
            "Install them with `python -m pip install -r apps/desktop/scripts/requirements-qwen-receipt.txt`."
        ) from error


@lru_cache(maxsize=1)
def load_model(model_id: str, local_dir: str | None = None):
    _require_transformers()
    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer

    model_ref = resolve_model_ref(model_id, local_dir)
    tokenizer = AutoTokenizer.from_pretrained(model_ref)
    model = AutoModelForCausalLM.from_pretrained(
        model_ref,
        torch_dtype="auto",  # uses BF16 natively (~0.8 GB vs ~1.6 GB for float32)
        # No device_map — requires `accelerate`; without it transformers defaults to CPU.
    )
    model.eval()
    return tokenizer, model


def resolve_model_ref(model_id: str, local_dir: str | None) -> str:
    if local_dir:
        local_path = Path(local_dir)
        if local_path.exists():
            return str(local_path)
    return model_id


def download_model(model_id: str, local_dir: str | None) -> Path | None:
    _require_transformers()
    from huggingface_hub import snapshot_download

    target_dir: Path | None = Path(local_dir) if local_dir else None
    if target_dir:
        target_dir.mkdir(parents=True, exist_ok=True)
        snapshot_download(
            repo_id=model_id,
            local_dir=str(target_dir),
            local_dir_use_symlinks=False,
        )
        (target_dir / ".download-complete").write_text(model_id, encoding="utf-8")
        return target_dir

    snapshot_download(repo_id=model_id)
    return None


def build_prompt(payload: dict) -> list[dict]:
    hints = {
        "merchant": payload.get("merchant"),
        "date": payload.get("date"),
        "total": payload.get("total"),
        "confidence": payload.get("confidence"),
        "used_total_fallback": payload.get("used_total_fallback"),
    }

    return [
        {
            "role": "system",
            "content": (
                "You repair noisy OCR text from shopping receipts. "
                "Return JSON only with keys merchant, description, total, date, confidence. "
                "Rules: do not invent values not supported by the OCR text; prefer explicit total/payment lines "
                "over line-item prices; output date in YYYY-MM-DD when possible; output total as a number; "
                "set unknown values to null; confidence must be between 0 and 1."
            ),
        },
        {
            "role": "user",
            "content": (
                "OCR text:\n"
                f"{payload.get('raw_text', '').strip()}\n\n"
                "Heuristic parser hints:\n"
                f"{json.dumps(hints, ensure_ascii=False)}\n\n"
                "Return JSON only."
            ),
        },
    ]


def extract_json(text: str) -> dict:
    text = text.strip()
    text = text.replace("```json", "```")
    if "```" in text:
        blocks = [segment.strip() for segment in text.split("```") if segment.strip()]
        for block in blocks:
            if block.startswith("{") and block.endswith("}"):
                return json.loads(block)

    match = re.search(r"\{.*\}", text, re.S)
    if not match:
        raise ValueError(f"No JSON object found in model output: {text[:400]}")
    return json.loads(match.group(0))


def normalize_output(data: dict) -> dict:
    merchant = data.get("merchant")
    description = data.get("description")
    date = data.get("date")
    total = data.get("total")
    confidence = data.get("confidence")

    if isinstance(total, str):
        total = total.replace(",", ".")
        try:
            total = float(total)
        except ValueError:
            total = None

    if isinstance(confidence, str):
        try:
            confidence = float(confidence)
        except ValueError:
            confidence = None

    if isinstance(confidence, (int, float)):
        confidence = max(0.0, min(1.0, float(confidence)))
    else:
        confidence = None

    return {
        "merchant": merchant.strip() if isinstance(merchant, str) and merchant.strip() else None,
        "description": description.strip() if isinstance(description, str) and description.strip() else None,
        "date": date.strip() if isinstance(date, str) and date.strip() else None,
        "total": total if isinstance(total, (int, float)) else None,
        "confidence": confidence,
    }


def run(model_id: str, payload: dict, local_dir: str | None = None) -> dict:
    tokenizer, model = load_model(model_id, local_dir)
    messages = build_prompt(payload)

    try:
        text = tokenizer.apply_chat_template(
            messages,
            tokenize=False,
            add_generation_prompt=True,
            enable_thinking=False,
        )
    except TypeError:
        text = tokenizer.apply_chat_template(
            messages,
            tokenize=False,
            add_generation_prompt=True,
        )

    t_tok = time.perf_counter()
    model_inputs = tokenizer([text], return_tensors="pt")
    tok_ms = (time.perf_counter() - t_tok) * 1000

    # Qwen3 docs: do NOT use greedy decoding (do_sample=False) — degrades quality.
    # Non-thinking mode recommended params: temperature=0.7, top_p=0.8, top_k=20.
    # max_new_tokens=80: the output JSON is ~40-50 tokens; 80 gives buffer without wasting CPU.
    t_gen = time.perf_counter()
    generated = model.generate(
        **model_inputs,
        max_new_tokens=80,
        do_sample=True,
        temperature=0.7,
        top_p=0.8,
        top_k=20,
        pad_token_id=tokenizer.eos_token_id,
    )
    gen_ms = (time.perf_counter() - t_gen) * 1000

    output_ids = generated[0][len(model_inputs.input_ids[0]) :]
    n_tokens = len(output_ids)
    output_text = tokenizer.decode(output_ids, skip_special_tokens=True).strip()
    print(
        f"[Timing] tokenize={tok_ms:.0f}ms  generate={gen_ms:.0f}ms  tokens={n_tokens}",
        file=sys.stderr,
        flush=True,
    )
    return normalize_output(extract_json(output_text))


def server_loop(model_id: str, local_dir: str | None) -> int:
    """Persistent server mode: read one JSON line per request, write one JSON line per response.

    The model is loaded once on startup (warm-up) so the first request does not pay the
    cold-start penalty.  Errors are returned as ``{"error": "<message>", ...null fields}``
    so the caller always receives a complete JSON response line.
    """
    # Warm-up: load model into lru_cache before accepting any requests.
    # Signal readiness to the Rust caller so it can separate load time from inference timeout.
    t_load = time.perf_counter()
    load_model(model_id, local_dir)
    load_ms = (time.perf_counter() - t_load) * 1000
    print(f"[Timing] Model warmup: {load_ms:.0f}ms", file=sys.stderr, flush=True)
    print(json.dumps({"status": "ready"}), flush=True)

    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue
        try:
            payload = json.loads(line)
            result = run(model_id, payload, local_dir)
        except Exception as exc:  # noqa: BLE001
            result = {
                "error": str(exc),
                "merchant": None,
                "description": None,
                "date": None,
                "total": None,
                "confidence": None,
            }
        # flush=True is critical: Rust reads line-by-line and will block if the buffer
        # is not flushed after each response.
        print(json.dumps(result, ensure_ascii=False), flush=True)

    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default="Qwen/Qwen3-0.6B")
    parser.add_argument("--local-dir")
    parser.add_argument("--download-only", action="store_true")
    parser.add_argument(
        "--server",
        action="store_true",
        help="Persistent server mode: read one JSON request per line from stdin, "
        "write one JSON response per line to stdout.",
    )
    args = parser.parse_args()

    if args.download_only:
        download_model(args.model, args.local_dir)
        return 0

    if args.server:
        return server_loop(args.model, args.local_dir)

    payload = json.load(sys.stdin)
    result = run(args.model, payload, args.local_dir)
    print(json.dumps(result, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

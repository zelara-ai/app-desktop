import { ChangeEvent, useEffect, useMemo, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { DEFAULT_CATEGORIES } from "../../../../src/packages/finance/src/schema";
import { categorizeTransaction } from "../../../../src/packages/finance/src/categorization";
import type {
  Category,
  CategorizationSource,
  FinanceContextSyncPayload,
} from "../../../../src/packages/finance/src/types";

type AiActivityEntry = {
  task_id: string;
  capability: string;
  summary: string;
  timestamp: string;
  status: string;
  error?: string | null;
  metadata: Record<string, unknown>;
};

type ImportedTransaction = {
  date: string;
  description: string;
  amount: number;
  currency: string;
  merchant?: string | null;
  source_format: string;
};

type ReceiptFieldEvidence = {
  sourceLines: string[];
  extractedValue: string;
  confidence: number;
  reason: string;
};

type ReceiptFieldSuggestions = {
  merchant: string[];
  amount: string[];
  date: string[];
  category: string[];
};

type ReceiptFieldConfidence = {
  amount?: number;
  total?: number;
  merchant: number;
  category?: number;
  date: number;
  lineQuality: number;
  imageQuality?: number;
  overall?: number;
};

type ReceiptJob = {
  receiptId: string;
  deviceId: string;
  status: string;
  capturedAt: string;
  updatedAt: string;
  retryCount: number;
  imagePath: string;
  mimeType: string;
  reviewReason?: string | null;
  reviewFields?: string[];
  readinessScore?: number | null;
  fieldConfidence?: ReceiptFieldConfidence | null;
  fieldEvidence?: {
    merchant?: ReceiptFieldEvidence;
    amount?: ReceiptFieldEvidence;
    date?: ReceiptFieldEvidence;
    category?: ReceiptFieldEvidence;
  } | null;
  fieldSuggestions?: ReceiptFieldSuggestions | null;
  draft?: {
    amount: number;
    currency: string;
    description: string;
    merchant?: string | null;
    date?: string | null;
    categoryId?: string | null;
  } | null;
  ocrResult?: Record<string, unknown> | null;
  error?: string | null;
};

type ReceiptLogEntry = {
  timestamp: string;
  jobId: string;
  stage: string;
  level: string;
  message: string;
  durationMs?: number | null;
  errorDetail?: string | null;
  variantScores?: Array<{ label: string; score: number; chars: number; selected: boolean }> | null;
};

type CategorySuggestionSource = CategorizationSource | "ocr";

type CategoryRecommendation = {
  categoryId: string;
  source: CategorySuggestionSource;
  matchedLabel?: string;
};

function formatCurrency(value: number, currency = "EUR") {
  return new Intl.NumberFormat(undefined, {
    style: "currency",
    currency,
    maximumFractionDigits: 2,
  }).format(value);
}

function sameDay(value: string) {
  return value.slice(0, 10) === new Date().toISOString().slice(0, 10);
}

export default function Finance() {
  const [activity, setActivity] = useState<AiActivityEntry[]>([]);
  const [receiptJobs, setReceiptJobs] = useState<ReceiptJob[]>([]);
  const [selectedReceiptId, setSelectedReceiptId] = useState<string | null>(
    null,
  );
  const [selectedActivityTaskId, setSelectedActivityTaskId] = useState<
    string | null
  >(null);
  const [financeContext, setFinanceContext] =
    useState<FinanceContextSyncPayload | null>(null);
  const [categoryAutoMode, setCategoryAutoMode] = useState<"open" | "locked">(
    "open",
  );
  const [draft, setDraft] = useState({
    amount: "",
    description: "",
    merchant: "",
    date: "",
    categoryId: "",
  });
  const [loadingActivity, setLoadingActivity] = useState(true);
  const [loadingReceipts, setLoadingReceipts] = useState(true);
  const [sendingDraft, setSendingDraft] = useState(false);
  const [receiptMessage, setReceiptMessage] = useState<string | null>(null);
  const [receiptLog, setReceiptLog] = useState<ReceiptLogEntry[]>([]);
  const [receiptImageFailed, setReceiptImageFailed] = useState(false);
  const [logExpanded, setLogExpanded] = useState(false);
  const [ocrTestPath, setOcrTestPath] = useState("");
  const [ocrTestRunning, setOcrTestRunning] = useState(false);
  const [ocrTestResult, setOcrTestResult] = useState<unknown>(null);
  const [ocrTestError, setOcrTestError] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);
  const [syncMessage, setSyncMessage] = useState<string | null>(null);
  const [importedTransactions, setImportedTransactions] = useState<
    ImportedTransaction[]
  >([]);
  const [selectedRows, setSelectedRows] = useState<Record<number, boolean>>({});

  useEffect(() => {
    void Promise.all([loadActivity(), loadReceiptJobs(), loadFinanceContext()]);
    void requestFinanceContextSync();

    const unlisteners: Array<() => void> = [];
    listen<AiActivityEntry>("ai-activity-updated", (event) => {
      setLoadingActivity(false);
      setActivity((current) => upsertByTaskId(current, event.payload));
    }).then((unlisten) => unlisteners.push(unlisten));
    listen<ReceiptJob>("receipt-job-updated", (event) => {
      setLoadingReceipts(false);
      setReceiptJobs((current) => upsertReceiptJob(current, event.payload));
    }).then((unlisten) => unlisteners.push(unlisten));
    listen<ReceiptJob[]>("receipt-jobs-updated", (event) => {
      setLoadingReceipts(false);
      setReceiptJobs(
        event.payload
          .slice()
          .sort((left, right) => right.updatedAt.localeCompare(left.updatedAt)),
      );
    }).then((unlisten) => unlisteners.push(unlisten));
    listen<FinanceContextSyncPayload>("finance-context-updated", (event) => {
      setFinanceContext(event.payload);
    }).then((unlisten) => unlisteners.push(unlisten));

    return () => {
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, []);

  const visibleReceiptJobs = useMemo(
    () =>
      receiptJobs.filter(
        (job) => job.status !== "saved" && job.status !== "corrupt",
      ),
    [receiptJobs],
  );

  const corruptReceiptJobs = useMemo(
    () => receiptJobs.filter((job) => job.status === "corrupt"),
    [receiptJobs],
  );

  useEffect(() => {
    if (!visibleReceiptJobs.length) {
      setSelectedReceiptId(null);
      return;
    }

    if (
      !selectedReceiptId ||
      !visibleReceiptJobs.some((job) => job.receiptId === selectedReceiptId)
    ) {
      setSelectedReceiptId(visibleReceiptJobs[0].receiptId);
    }
  }, [visibleReceiptJobs, selectedReceiptId]);

  const selectedReceipt = useMemo(
    () =>
      visibleReceiptJobs.find((job) => job.receiptId === selectedReceiptId) ??
      visibleReceiptJobs[0] ??
      null,
    [visibleReceiptJobs, selectedReceiptId],
  );

  const selectedActivity = useMemo(
    () =>
      activity.find((entry) => entry.task_id === selectedActivityTaskId) ??
      null,
    [activity, selectedActivityTaskId],
  );

  const previewReceipt = useMemo(() => {
    if (!selectedActivity || selectedActivity.capability !== "ocr_receipt") {
      return null;
    }
    return (
      visibleReceiptJobs.find(
        (job) => job.receiptId === selectedActivity.task_id,
      ) ?? null
    );
  }, [selectedActivity, visibleReceiptJobs]);

  useEffect(() => {
    if (!selectedReceipt) {
      setDraft({
        amount: "",
        description: "",
        merchant: "",
        date: "",
        categoryId: "",
      });
      setCategoryAutoMode("open");
      setReceiptLog([]);
      return;
    }

    void loadReceiptLog(selectedReceipt.receiptId);

    setDraft({
      amount:
        selectedReceipt.draft?.amount != null
          ? selectedReceipt.draft.amount.toFixed(2)
          : "",
      description: selectedReceipt.draft?.description ?? "",
      merchant: selectedReceipt.draft?.merchant ?? "",
      date: selectedReceipt.draft?.date ?? "",
      categoryId: selectedReceipt.draft?.categoryId ?? "",
    });
    setCategoryAutoMode(
      selectedReceipt.draft?.categoryId ? "locked" : "open",
    );
  }, [selectedReceipt]);

  useEffect(() => {
    setReceiptImageFailed(false);
  }, [
    previewReceipt?.receiptId,
    previewReceipt?.imagePath,
    selectedReceipt?.receiptId,
    selectedReceipt?.imagePath,
  ]);

  useEffect(() => {
    if (!selectedActivityTaskId) {
      return;
    }
    if (!selectedActivity || !previewReceipt) {
      setSelectedActivityTaskId(null);
    }
  }, [selectedActivityTaskId, selectedActivity, previewReceipt]);

  useEffect(() => {
    if (
      previewReceipt &&
      selectedReceiptId !== previewReceipt.receiptId
    ) {
      setSelectedReceiptId(previewReceipt.receiptId);
    }
  }, [previewReceipt, selectedReceiptId]);

  const summary = useMemo(() => {
    const receiptsToday = activity.filter(
      (entry) =>
        entry.capability === "ocr_receipt" &&
        (entry.status === "completed" || entry.status === "needs_review") &&
        sameDay(entry.timestamp),
    ).length;
    const totalScanned = activity
      .filter(
        (entry) =>
          entry.capability === "ocr_receipt" && sameDay(entry.timestamp),
      )
      .filter((entry) => entry.status === "completed")
      .reduce((sum, entry) => {
        const total = Number(entry.metadata?.total ?? 0);
        return Number.isFinite(total) ? sum + Math.abs(total) : sum;
      }, 0);

    return {
      receiptsToday,
      totalScanned,
      needsReview: visibleReceiptJobs.filter(
        (job) => job.status === "needs_review",
      ).length,
      readyDrafts: visibleReceiptJobs.filter(
        (job) => job.status === "completed",
      ).length,
    };
  }, [activity, visibleReceiptJobs]);

  async function loadActivity() {
    setLoadingActivity(true);
    try {
      const result = await invoke<AiActivityEntry[]>("get_ai_activity");
      setActivity(
        result
          .slice()
          .sort((left, right) => right.timestamp.localeCompare(left.timestamp)),
      );
    } finally {
      setLoadingActivity(false);
    }
  }

  async function loadReceiptLog(jobId: string) {
    try {
      const entries = await invoke<ReceiptLogEntry[]>("get_receipt_log", {
        jobId,
      });
      setReceiptLog(entries);
    } catch {
      setReceiptLog([]);
    }
  }

  async function loadReceiptJobs() {
    setLoadingReceipts(true);
    try {
      const result = await invoke<ReceiptJob[]>("get_receipt_jobs");
      const sorted = result
        .slice()
        .sort((left, right) => right.updatedAt.localeCompare(left.updatedAt));
      setReceiptJobs(sorted);
      return sorted;
    } finally {
      setLoadingReceipts(false);
    }
  }

  async function loadFinanceContext() {
    try {
      const result = await invoke<FinanceContextSyncPayload | null>(
        "get_finance_context",
      );
      setFinanceContext(result);
    } catch {
      setFinanceContext(null);
    }
  }

  async function requestFinanceContextSync() {
    try {
      await invoke("request_finance_context_sync");
    } catch {
      // Desktop can still fall back to defaults when no mobile client is connected.
    }
  }

  async function handleOcrTest() {
    if (!ocrTestPath.trim()) return;
    setOcrTestRunning(true);
    setOcrTestResult(null);
    setOcrTestError(null);
    try {
      const result = await invoke("test_ocr_pipeline", {
        imagePath: ocrTestPath.trim(),
      });
      setOcrTestResult(result);
    } catch (error) {
      setOcrTestError(
        error instanceof Error ? error.message : String(error),
      );
    } finally {
      setOcrTestRunning(false);
    }
  }

  async function handleSendReceiptDraft() {
    if (!selectedReceipt) {
      return;
    }

    const currentIndex = visibleReceiptJobs.findIndex(
      (job) => job.receiptId === selectedReceipt.receiptId,
    );
    const nextReviewableReceiptId =
      visibleReceiptJobs[currentIndex + 1]?.receiptId ??
      visibleReceiptJobs[currentIndex - 1]?.receiptId ??
      null;

    setSendingDraft(true);
    setReceiptMessage(null);
    try {
      const connectedClients = await invoke<number>(
        "send_receipt_job_to_mobile",
        {
          receiptId: selectedReceipt.receiptId,
          amount: Number(draft.amount) || 0,
          description: draft.description || "Receipt",
          merchant: draft.merchant || null,
          date: draft.date || null,
          categoryId: draft.categoryId || null,
        },
      );
      setReceiptMessage(
        connectedClients > 0
          ? `Draft sent to ${connectedClients} connected mobile client${connectedClients === 1 ? "" : "s"}.`
          : "No connected mobile clients were available.",
      );
      await loadReceiptJobs();
      setSelectedReceiptId(nextReviewableReceiptId);
    } catch (error) {
      setReceiptMessage(
        error instanceof Error
          ? error.message
          : "Could not send receipt draft.",
      );
    } finally {
      setSendingDraft(false);
    }
  }

  async function handleFilePick(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    if (!file) {
      return;
    }

    setImporting(true);
    setImportError(null);
    try {
      const path = (file as File & { path?: string }).path;
      if (!path) {
        throw new Error("The selected file path was not available to Tauri.");
      }

      const parsed = await invoke<ImportedTransaction[]>(
        "import_finance_file",
        {
          path,
        },
      );
      setImportedTransactions(parsed);
      setSelectedRows(
        Object.fromEntries(parsed.map((_, index) => [index, true])),
      );
      setSyncMessage(null);
      await loadActivity();
    } catch (error) {
      setImportedTransactions([]);
      setImportError(error instanceof Error ? error.message : "Import failed.");
    } finally {
      setImporting(false);
      event.target.value = "";
    }
  }

  async function handleSendToFinanceApp() {
    const transactions = importedTransactions.filter(
      (_, index) => selectedRows[index],
    );
    if (transactions.length === 0) {
      setSyncMessage("Select at least one imported row before sending.");
      return;
    }

    setSyncing(true);
    setSyncMessage(null);
    try {
      const connectedClients = await invoke<number>(
        "push_finance_transactions",
        {
          transactions,
        },
      );
      setSyncMessage(
        connectedClients > 0
          ? `Sent ${transactions.length} transaction${transactions.length === 1 ? "" : "s"} to ${connectedClients} connected mobile client${connectedClients === 1 ? "" : "s"}.`
          : "No mobile clients are currently connected, so nothing was pushed.",
      );
    } catch (error) {
      setSyncMessage(
        error instanceof Error ? error.message : "Finance sync push failed.",
      );
    } finally {
      setSyncing(false);
    }
  }

  function toggleRow(index: number) {
    setSelectedRows((current) => ({
      ...current,
      [index]: !current[index],
    }));
  }

  function handleActivitySelection(entry: AiActivityEntry) {
    const nextTaskId =
      selectedActivityTaskId === entry.task_id ? null : entry.task_id;
    setSelectedActivityTaskId(nextTaskId);
  }

  function setDraftField(
    field: "amount" | "description" | "merchant" | "date" | "categoryId",
    value: string,
  ) {
    if (
      field === "merchant" ||
      field === "description" ||
      field === "categoryId"
    ) {
      setCategoryAutoMode("locked");
    }
    setDraft((current) => ({
      ...current,
      [field]: value,
    }));
  }

  const selectedFieldEvidence = selectedReceipt?.fieldEvidence ?? null;
  const selectedFieldSuggestions = selectedReceipt?.fieldSuggestions ?? null;

  const selectedConfidence = selectedReceipt?.fieldConfidence;
  const amountConfidence =
    selectedConfidence?.amount ?? selectedConfidence?.total ?? 0;

  const selectedReceiptTitle = selectedReceipt
    ? buildReceiptTitle(selectedReceipt)
    : "Receipt pending review";
  const selectedReceiptSubtitle = selectedReceipt
    ? buildReceiptSubtitle(selectedReceipt)
    : "";
  const financeCategories = useMemo(
    () =>
      financeContext?.categories?.length
        ? financeContext.categories
        : DEFAULT_CATEGORIES,
    [financeContext],
  );
  const categoryRecommendation = useMemo(
    () =>
      buildCategoryRecommendation({
        amount: Number(draft.amount) || selectedReceipt?.draft?.amount || 0,
        categories: financeCategories,
        draft,
        financeContext,
        receipt: selectedReceipt,
      }),
    [draft, financeCategories, financeContext, selectedReceipt],
  );
  const suggestedCategory = useMemo(
    () =>
      categoryRecommendation.categoryId &&
      categoryRecommendation.categoryId !== draft.categoryId
        ? findCategoryById(financeCategories, categoryRecommendation.categoryId)
        : null,
    [categoryRecommendation, draft.categoryId, financeCategories],
  );
  const categorySuggestionCopy = useMemo(
    () => describeCategoryRecommendation(categoryRecommendation, suggestedCategory),
    [categoryRecommendation, suggestedCategory],
  );

  useEffect(() => {
    if (
      !selectedReceipt ||
      categoryAutoMode !== "open" ||
      !categoryRecommendation.categoryId ||
      draft.categoryId === categoryRecommendation.categoryId
    ) {
      return;
    }

    setDraft((current) => ({
      ...current,
      categoryId: categoryRecommendation.categoryId,
    }));
  }, [categoryAutoMode, categoryRecommendation, draft.categoryId, selectedReceipt]);

  return (
    <section className="finance-dashboard">
      <div className="finance-header">
        <div>
          <h2>Finance Overview</h2>
          <p>
            Queue receipts on mobile, review them on desktop, and push the
            approved draft back into Finance.
          </p>
        </div>
        <div className="finance-header__actions">
          <button onClick={() => void loadReceiptJobs()} type="button">
            Refresh Inbox
          </button>
          <button onClick={() => void loadActivity()} type="button">
            Refresh Activity
          </button>
        </div>
      </div>

      <div className="finance-summary-grid">
        <article className="finance-card">
          <span className="finance-card__label">Receipts today</span>
          <strong>{summary.receiptsToday}</strong>
          <p>Receipt OCR jobs logged through the desktop runtime today.</p>
        </article>
        <article className="finance-card">
          <span className="finance-card__label">Ready drafts</span>
          <strong>{summary.readyDrafts}</strong>
          <p>
            High-confidence receipts waiting for a final desktop confirmation.
          </p>
        </article>
        <article className="finance-card">
          <span className="finance-card__label">Needs review</span>
          <strong>{summary.needsReview}</strong>
          <p>Receipts that did not clear the 0.85 readiness target.</p>
        </article>
        <article className="finance-card">
          <span className="finance-card__label">Total scanned</span>
          <strong>{formatCurrency(summary.totalScanned)}</strong>
          <p>
            Approved OCR totals recognized by Zelara Core across today&apos;s
            receipts.
          </p>
        </article>
      </div>

      <details className="finance-ocr-test">
        <summary>OCR Smoke Test</summary>
        <div className="finance-ocr-test__body">
          <label className="receipt-review__field">
            <span>Receipt image path</span>
            <input
              type="text"
              placeholder="Absolute path to a receipt image (.jpg / .png)…"
              value={ocrTestPath}
              onChange={(e) => setOcrTestPath(e.target.value)}
            />
          </label>
          <button
            disabled={ocrTestRunning || !ocrTestPath.trim()}
            type="button"
            onClick={() => void handleOcrTest()}
          >
            {ocrTestRunning ? "Running…" : "Run Test"}
          </button>
          {ocrTestError ? (
            <p className="finance-error">{ocrTestError}</p>
          ) : null}
          {ocrTestResult != null ? (
            <pre className="finance-ocr-test__result">
              {JSON.stringify(ocrTestResult, null, 2)}
            </pre>
          ) : null}
        </div>
      </details>

      <article className="finance-panel finance-panel--review">
          <div className="finance-panel__header">
            <h3>Receipt Review Inbox</h3>
            <span>{visibleReceiptJobs.length} receipts</span>
          </div>
          {corruptReceiptJobs.length > 0 ? (
            <p className="finance-note">
              {corruptReceiptJobs.length} corrupt receipt
              {corruptReceiptJobs.length === 1 ? "" : "s"} were quarantined
              from review automatically.
            </p>
          ) : null}
          {loadingReceipts ? (
            <p className="finance-empty">Loading receipt inbox…</p>
          ) : visibleReceiptJobs.length === 0 ? (
            corruptReceiptJobs.length > 0 ? (
              <p className="finance-empty">
                No reviewable receipts right now. The stale or unreadable queue
                items were flagged as corrupt and removed from this inbox.
              </p>
            ) : (
              <p className="finance-empty">
                No queued receipts yet. Capture a few in the mobile app and
                reconnect.
              </p>
            )
          ) : (
            <div className="receipt-review">
              <aside className="receipt-review__list">
                {visibleReceiptJobs.map((job) => (
                  <button
                    key={job.receiptId}
                    type="button"
                    className={`receipt-review__list-item${job.receiptId === selectedReceipt?.receiptId ? " is-active" : ""}`}
                    onClick={() => setSelectedReceiptId(job.receiptId)}
                  >
                    <div className="receipt-review__list-head">
                      <strong className="receipt-review__list-title">
                        {buildReceiptTitle(job)}
                      </strong>
                      <span
                        className={`finance-activity-status finance-activity-status--${job.status}`}
                      >
                        {job.status.replace(/_/g, " ")}
                      </span>
                    </div>
                    <span className="receipt-review__list-subtitle">
                      {buildReceiptSubtitle(job)}
                    </span>
                    <small>{new Date(job.updatedAt).toLocaleString()}</small>
                    <div className="receipt-review__list-meta">
                      <span>
                        Readiness{" "}
                        {typeof job.readinessScore === "number"
                          ? job.readinessScore.toFixed(2)
                          : "n/a"}
                      </span>
                      {job.reviewFields?.length ? (
                        <span>
                          Needs review:{" "}
                          {job.reviewFields
                            .map((field) => humanizeReviewField(field))
                            .join(", ")}
                        </span>
                      ) : null}
                    </div>
                  </button>
                ))}
              </aside>

              {selectedReceipt ? (
                <div className="receipt-review__detail">
                  <div className="receipt-review__evidence">
                    <div className="receipt-review__evidence-header">
                      <div>
                        <h4>{selectedReceiptTitle}</h4>
                        <p>{selectedReceiptSubtitle}</p>
                      </div>
                      <div className="receipt-review__evidence-badges">
                        <span
                          className={`finance-activity-status finance-activity-status--${selectedReceipt.status}`}
                        >
                          {selectedReceipt.status.replace(/_/g, " ")}
                        </span>
                        <span className="receipt-review__score-pill">
                          Readiness{" "}
                          {typeof selectedReceipt.readinessScore === "number"
                            ? selectedReceipt.readinessScore.toFixed(2)
                            : "n/a"}
                        </span>
                      </div>
                    </div>

                    <div className="receipt-review__image-panel">
                      {receiptImageFailed ? (
                        <p className="finance-error">
                          Could not load the queued receipt image from disk.
                        </p>
                      ) : (
                        <img
                          alt={selectedReceiptTitle}
                          className="receipt-review__image"
                          onError={() => setReceiptImageFailed(true)}
                          src={convertFileSrc(selectedReceipt.imagePath)}
                        />
                      )}
                    </div>

                    <div className="receipt-review__evidence-grid">
                      {(
                        [
                          {
                            key: "merchant",
                            label: "Merchant evidence",
                            value:
                              selectedFieldEvidence?.merchant?.extractedValue ||
                              "Not extracted",
                            reason: selectedFieldEvidence?.merchant?.reason,
                            lines:
                              selectedFieldEvidence?.merchant?.sourceLines ?? [],
                          },
                          {
                            key: "amount",
                            label: "Amount evidence",
                            value: selectedFieldEvidence?.amount?.extractedValue
                              ? formatCurrency(
                                  Number(
                                    selectedFieldEvidence?.amount
                                      ?.extractedValue ?? 0,
                                  ),
                                )
                              : "Not extracted",
                            reason: selectedFieldEvidence?.amount?.reason,
                            lines:
                              selectedFieldEvidence?.amount?.sourceLines ?? [],
                          },
                          {
                            key: "date",
                            label: "Date evidence",
                            value:
                              selectedFieldEvidence?.date?.extractedValue ||
                              "Not extracted",
                            reason: selectedFieldEvidence?.date?.reason,
                            lines: selectedFieldEvidence?.date?.sourceLines ?? [],
                          },
                          {
                            key: "category",
                            label: "Category evidence",
                            value:
                              selectedFieldEvidence?.category?.extractedValue ||
                              "Needs manual choice",
                            reason: selectedFieldEvidence?.category?.reason,
                            lines:
                              selectedFieldEvidence?.category?.sourceLines ?? [],
                          },
                        ] as const
                      ).map((section) => (
                        <article
                          key={section.key}
                          className="receipt-evidence-card"
                        >
                          <span className="receipt-evidence-card__label">
                            {section.label}
                          </span>
                          <strong>{section.value}</strong>
                          {section.reason ? <p>{section.reason}</p> : null}
                          {section.lines.length ? (
                            <ul className="receipt-evidence-card__lines">
                              {section.lines.map((line, index) => (
                                <li key={`${section.key}-${index}`}>{line}</li>
                              ))}
                            </ul>
                          ) : null}
                        </article>
                      ))}
                    </div>
                  </div>

                  <div className="receipt-review__editor">
                    <div className="receipt-review__meta">
                      <span className="receipt-review__meta-label">
                        Side-by-side verify
                      </span>
                      {selectedReceipt.reviewFields?.length ? (
                        <span className="receipt-review__meta-label">
                          Needs review:{" "}
                          {selectedReceipt.reviewFields
                            .map((field) => humanizeReviewField(field))
                            .join(", ")}
                        </span>
                      ) : null}
                    </div>

                    {selectedReceipt.reviewReason ? (
                      <p className="receipt-review__warning">
                        {selectedReceipt.reviewReason}
                      </p>
                    ) : null}
                    {selectedReceipt.error ? (
                      <p className="finance-activity-error">
                        {selectedReceipt.error}
                      </p>
                    ) : null}

                    <label className="receipt-review__field">
                      <span>Amount</span>
                      <input
                        type="number"
                        step="0.01"
                        value={draft.amount}
                        onChange={(event) =>
                          setDraftField("amount", event.target.value)
                        }
                      />
                      <small className="receipt-review__field-note">
                        AI saw:{" "}
                        {selectedFieldEvidence?.amount?.sourceLines?.join(" • ") ||
                          "No total line captured"}
                      </small>
                      <small className="receipt-review__field-note">
                        {selectedFieldEvidence?.amount?.reason ||
                          "Review the receipt total before saving."}
                      </small>
                      {selectedReceipt.reviewFields?.includes("amount") ? (
                        <p className="receipt-review__field-warning">
                          Amount needs review.
                        </p>
                      ) : null}
                      {selectedFieldSuggestions?.amount?.length ? (
                        <div className="receipt-review__chips">
                          {selectedFieldSuggestions.amount.map((value) => (
                            <button
                              key={value}
                              type="button"
                              className="receipt-review__chip"
                              onClick={() => setDraftField("amount", value)}
                            >
                              {formatCurrency(Number(value))}
                            </button>
                          ))}
                        </div>
                      ) : null}
                    </label>
                    <label className="receipt-review__field">
                      <span>Description</span>
                      <input
                        type="text"
                        value={draft.description}
                        onChange={(event) =>
                          setDraftField("description", event.target.value)
                        }
                      />
                      <small className="receipt-review__field-note">
                        Defaulted from the normalized merchant so the queue does
                        not surface raw OCR fragments.
                      </small>
                    </label>
                    <label className="receipt-review__field">
                      <span>Merchant</span>
                      <input
                        type="text"
                        value={draft.merchant}
                        onChange={(event) =>
                          setDraftField("merchant", event.target.value)
                        }
                      />
                      <small className="receipt-review__field-note">
                        AI saw:{" "}
                        {selectedFieldEvidence?.merchant?.sourceLines?.join(
                          " • ",
                        ) || "No merchant header captured"}
                      </small>
                      <small className="receipt-review__field-note">
                        {selectedFieldEvidence?.merchant?.reason ||
                          "Merchant was inferred from the header."}
                      </small>
                      {selectedReceipt.reviewFields?.includes("merchant") ? (
                        <p className="receipt-review__field-warning">
                          Merchant needs review.
                        </p>
                      ) : null}
                      {selectedFieldSuggestions?.merchant?.length ? (
                        <div className="receipt-review__chips">
                          {selectedFieldSuggestions.merchant.map((value) => (
                            <button
                              key={value}
                              type="button"
                              className="receipt-review__chip"
                              onClick={() => {
                                setDraftField("merchant", value);
                                if (
                                  !draft.description.trim() ||
                                  draft.description === "Receipt pending review"
                                ) {
                                  setDraftField(
                                    "description",
                                    `${value} receipt`,
                                  );
                                }
                              }}
                            >
                              {value}
                            </button>
                          ))}
                        </div>
                      ) : null}
                    </label>
                    <label className="receipt-review__field">
                      <span>Date</span>
                      <input
                        type="text"
                        value={draft.date}
                        onChange={(event) =>
                          setDraftField("date", event.target.value)
                        }
                      />
                      <small className="receipt-review__field-note">
                        AI saw:{" "}
                        {selectedFieldEvidence?.date?.sourceLines?.join(" • ") ||
                          "No date line captured"}
                      </small>
                      <small className="receipt-review__field-note">
                        {selectedFieldEvidence?.date?.reason ||
                          "Date was not extracted confidently."}
                      </small>
                      {selectedFieldSuggestions?.date?.length ? (
                        <div className="receipt-review__chips">
                          {selectedFieldSuggestions.date.map((value) => (
                            <button
                              key={value}
                              type="button"
                              className="receipt-review__chip"
                              onClick={() => setDraftField("date", value)}
                            >
                              {value}
                            </button>
                          ))}
                        </div>
                      ) : null}
                    </label>
                    <label className="receipt-review__field">
                      <span>Category</span>
                      {categorySuggestionCopy ? (
                        <button
                          type="button"
                          className="receipt-review__category-suggestion"
                          onClick={() => {
                            if (suggestedCategory) {
                              setDraftField("categoryId", suggestedCategory.id);
                            }
                          }}
                        >
                          <span className="receipt-review__category-suggestion-label">
                            {categorySuggestionCopy.title}
                          </span>
                          <span>{categorySuggestionCopy.body}</span>
                        </button>
                      ) : null}
                      <div className="receipt-review__category-grid">
                        {financeCategories.map((category) => {
                          const selected = category.id === draft.categoryId;
                          return (
                            <button
                              key={category.id}
                              type="button"
                              className={`receipt-review__category-chip${selected ? " is-selected" : ""}`}
                              style={
                                selected
                                  ? {
                                      borderColor: category.color,
                                      backgroundColor: `${category.color}1a`,
                                    }
                                  : undefined
                              }
                              onClick={() => setDraftField("categoryId", category.id)}
                            >
                              <span
                                className="receipt-review__category-icon"
                                style={{ backgroundColor: `${category.color}22` }}
                              >
                                {category.icon}
                              </span>
                              <span
                                className="receipt-review__category-text"
                                style={selected ? { color: category.color } : undefined}
                              >
                                {category.name}
                              </span>
                            </button>
                          );
                        })}
                      </div>
                      <small className="receipt-review__field-note">
                        AI saw:{" "}
                        {selectedFieldEvidence?.category?.sourceLines?.join(
                          " • ",
                        ) || "No item evidence captured"}
                      </small>
                      <small className="receipt-review__field-note">
                        {selectedFieldEvidence?.category?.reason ||
                          "Category can follow your history, detected keywords, or OCR fallback."}
                      </small>
                    </label>

                    {selectedReceipt.fieldConfidence ? (
                      <div className="receipt-confidence">
                        <span>
                          Merchant{" "}
                          {(selectedReceipt.fieldConfidence.merchant * 100).toFixed(0)}%
                        </span>
                        <span>
                          Amount {(amountConfidence * 100).toFixed(0)}%
                        </span>
                        <span>
                          Date{" "}
                          {(selectedReceipt.fieldConfidence.date * 100).toFixed(0)}%
                        </span>
                        <span>
                          Line quality{" "}
                          {(selectedReceipt.fieldConfidence.lineQuality * 100).toFixed(0)}%
                        </span>
                      </div>
                    ) : null}

                    {receiptMessage ? (
                      <p className="finance-sync-message">{receiptMessage}</p>
                    ) : null}
                    <button
                      disabled={
                        sendingDraft ||
                        !draft.description.trim() ||
                        !draft.amount
                      }
                      type="button"
                      onClick={() => void handleSendReceiptDraft()}
                    >
                      {sendingDraft
                        ? "Sending draft…"
                        : "Send Draft to Finance app"}
                    </button>

                    <div className="receipt-log">
                      <button
                        type="button"
                        className="receipt-log__toggle"
                        onClick={() => {
                          setLogExpanded((current) => !current);
                          if (!logExpanded) {
                            void loadReceiptLog(selectedReceipt.receiptId);
                          }
                        }}
                      >
                        {logExpanded ? "▲" : "▼"} Diagnostics
                        {receiptLog.length > 0
                          ? ` (${receiptLog.length} entries)`
                          : ""}
                      </button>

                      {logExpanded ? (
                        <ul className="receipt-log__list">
                          {receiptLog.length === 0 ? (
                            <li className="receipt-log__empty">
                              No pipeline log entries for this receipt.
                            </li>
                          ) : (
                            receiptLog.map((entry, index) => (
                              <li
                                key={`${entry.timestamp}-${index}`}
                                className={`receipt-log__entry receipt-log__entry--${entry.level.toLowerCase()}`}
                              >
                                <div className="receipt-log__entry-header">
                                  <span className="receipt-log__stage">
                                    {entry.stage}
                                  </span>
                                  {entry.durationMs != null ? (
                                    <span className="receipt-log__duration">
                                      {entry.durationMs}ms
                                    </span>
                                  ) : null}
                                  <span className="receipt-log__level">
                                    {entry.level}
                                  </span>
                                </div>
                                <p className="receipt-log__message">
                                  {entry.message}
                                </p>
                                {entry.errorDetail ? (
                                  <p className="receipt-log__error">
                                    {entry.errorDetail}
                                  </p>
                                ) : null}
                                {entry.variantScores?.length ? (
                                  <ul className="receipt-log__variants">
                                    {entry.variantScores.map((variant) => (
                                      <li
                                        key={variant.label}
                                        className={
                                          variant.selected
                                            ? "receipt-log__variant--selected"
                                            : undefined
                                        }
                                      >
                                        {variant.label}: score {variant.score},{" "}
                                        {variant.chars} chars
                                        {variant.selected ? " ✓" : ""}
                                      </li>
                                    ))}
                                  </ul>
                                ) : null}
                              </li>
                            ))
                          )}
                        </ul>
                      ) : null}
                    </div>
                  </div>
                </div>
              ) : null}
            </div>
          )}
      </article>

      <div className="finance-panels">
        <article className="finance-panel finance-panel--activity">
          {previewReceipt ? (
            <div className="finance-activity-preview" role="dialog" aria-label="Receipt preview">
              <button
                type="button"
                className="finance-activity-preview__close"
                onClick={() => setSelectedActivityTaskId(null)}
              >
                Close
              </button>
              <div className="finance-activity-preview__frame">
                {receiptImageFailed ? (
                  <p className="finance-error">
                    Could not load the queued receipt image from disk.
                  </p>
                ) : (
                  <img
                    alt={previewReceipt.draft?.description || "Queued receipt"}
                    className="finance-activity-preview__image"
                    onError={() => setReceiptImageFailed(true)}
                    src={convertFileSrc(previewReceipt.imagePath)}
                  />
                )}
              </div>
              <div className="finance-activity-preview__meta">
                <strong>
                  {previewReceipt.draft?.description || "Queued receipt"}
                </strong>
                <span>{previewReceipt.status.replace(/_/g, " ")}</span>
              </div>
            </div>
          ) : null}
          <div className="finance-panel__header">
            <h3>Recent AI Activity</h3>
            <span>{activity.length} entries</span>
          </div>
          {loadingActivity ? (
            <p className="finance-empty">Loading activity…</p>
          ) : activity.length === 0 ? (
            <p className="finance-empty">
              No finance AI activity yet. Scan a receipt from the mobile app to
              populate this list.
            </p>
          ) : (
            <ul className="finance-activity-list">
              {activity.map((entry, index) => (
                <li
                  key={entry.task_id || `${entry.timestamp}-${index}`}
                  className={`finance-activity-row${entry.task_id === selectedActivityTaskId ? " is-active" : ""}${entry.capability === "ocr_receipt" &&
                  visibleReceiptJobs.some(
                    (job) => job.receiptId === entry.task_id,
                  )
                    ? " is-previewable"
                    : ""}`}
                >
                  <button
                    type="button"
                    className="finance-activity-button"
                    onClick={() => handleActivitySelection(entry)}
                  >
                    <div>
                      <div className="finance-activity-row__top">
                        <strong>{entry.summary}</strong>
                        <span
                          className={`finance-activity-status finance-activity-status--${entry.status}`}
                        >
                          {entry.status.replace(/_/g, " ")}
                        </span>
                      </div>
                      <p>{entry.capability}</p>
                      {typeof entry.metadata?.reviewReason === "string" ? (
                        <p>{entry.metadata.reviewReason}</p>
                      ) : null}
                      {typeof entry.metadata?.qwenFallback === "object" &&
                      entry.metadata?.qwenFallback &&
                      typeof (
                        entry.metadata.qwenFallback as { outcome?: unknown }
                      ).outcome === "string" &&
                      (entry.metadata.qwenFallback as { outcome?: string })
                        .outcome !== "not_needed" ? (
                        <p>
                          Qwen fallback:{" "}
                          {(
                            entry.metadata.qwenFallback as {
                              outcome?: string;
                            }
                          ).outcome?.replace(/_/g, " ")}
                        </p>
                      ) : null}
                      {entry.error ? (
                        <p className="finance-activity-error">{entry.error}</p>
                      ) : null}
                    </div>
                    <time>{new Date(entry.timestamp).toLocaleString()}</time>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </article>

        <article className="finance-panel">
          <div className="finance-panel__header">
            <h3>Import Bank File</h3>
            <span>CSV / OFX / XLSX</span>
          </div>

          <label className="finance-import-dropzone">
            <input
              accept=".csv,.ofx,.qfx,.xlsx,.xls"
              className="finance-import-input"
              onChange={handleFilePick}
              type="file"
            />
            <span>
              {importing ? "Parsing file…" : "Choose a statement file"}
            </span>
            <small>
              Tauri passes the local file path to the Rust parser for preview.
            </small>
          </label>

          {importError ? <p className="finance-error">{importError}</p> : null}

          {importedTransactions.length > 0 ? (
            <div className="finance-import-table-wrap">
              <div className="finance-import-toolbar">
                <div>
                  <strong>
                    {Object.values(selectedRows).filter(Boolean).length}
                  </strong>{" "}
                  selected
                </div>
                <button
                  disabled={syncing}
                  onClick={() => void handleSendToFinanceApp()}
                  type="button"
                >
                  {syncing ? "Sending…" : "Send to Finance app"}
                </button>
              </div>
              {syncMessage ? (
                <p className="finance-sync-message">{syncMessage}</p>
              ) : null}
              <table className="finance-import-table">
                <thead>
                  <tr>
                    <th>Send</th>
                    <th>Date</th>
                    <th>Description</th>
                    <th>Merchant</th>
                    <th>Amount</th>
                  </tr>
                </thead>
                <tbody>
                  {importedTransactions.map((transaction, index) => (
                    <tr key={`${transaction.date}-${index}`}>
                      <td>
                        <input
                          checked={!!selectedRows[index]}
                          onChange={() => toggleRow(index)}
                          type="checkbox"
                        />
                      </td>
                      <td>{transaction.date}</td>
                      <td>{transaction.description}</td>
                      <td>{transaction.merchant ?? "—"}</td>
                      <td>
                        {formatCurrency(
                          transaction.amount,
                          transaction.currency,
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : null}
        </article>
      </div>
    </section>
  );
}

function upsertByTaskId(current: AiActivityEntry[], entry: AiActivityEntry) {
  const next = [...current];
  const index = next.findIndex(
    (existing) => existing.task_id === entry.task_id,
  );
  if (index >= 0) {
    next[index] = entry;
  } else {
    next.unshift(entry);
  }
  return next.sort((left, right) =>
    right.timestamp.localeCompare(left.timestamp),
  );
}

function upsertReceiptJob(current: ReceiptJob[], entry: ReceiptJob) {
  const next = [...current];
  const index = next.findIndex(
    (existing) => existing.receiptId === entry.receiptId,
  );
  if (index >= 0) {
    next[index] = entry;
  } else {
    next.unshift(entry);
  }
  return next.sort((left, right) =>
    right.updatedAt.localeCompare(left.updatedAt),
  );
}

function buildReceiptTitle(job: ReceiptJob) {
  const merchant = job.draft?.merchant?.trim();
  const merchantConfidence = job.fieldConfidence?.merchant ?? 0;
  if (merchant && merchantConfidence >= 0.65) {
    return merchant;
  }
  if (job.status === "queued" || job.status === "running") {
    return "Queued receipt";
  }
  return "Receipt pending review";
}

function buildReceiptSubtitle(job: ReceiptJob) {
  const amount =
    job.draft?.amount && job.draft.amount > 0
      ? formatCurrency(job.draft.amount, job.draft?.currency || "EUR")
      : "Amount needs review";
  const date = job.draft?.date?.trim() || "Date missing";
  return `${amount} • ${date}`;
}

function humanizeReviewField(field: string) {
  switch (field) {
    case "amount":
      return "amount";
    case "merchant":
      return "merchant";
    case "date":
      return "date";
    case "category":
      return "category";
    case "text_quality":
      return "text quality";
    case "image_quality":
      return "image quality";
    default:
      return field.replace(/_/g, " ");
  }
}

function buildCategoryRecommendation({
  amount,
  categories,
  draft,
  financeContext,
  receipt,
}: {
  amount: number;
  categories: Category[];
  draft: {
    amount: string;
    description: string;
    merchant: string;
    date: string;
    categoryId: string;
  };
  financeContext: FinanceContextSyncPayload | null;
  receipt: ReceiptJob | null;
}): CategoryRecommendation {
  const preferredLabel = chooseCategoryLabel(draft);
  if (preferredLabel) {
    const result = categorizeTransaction({
      amount,
      description: preferredLabel,
      history: financeContext?.history ?? [],
    });
    if (result.source !== "none" && findCategoryById(categories, result.categoryId)) {
      return result;
    }
  }

  const ocrCategory =
    readStringRecordValue(receipt?.ocrResult, "categoryId") ??
    receipt?.fieldSuggestions?.category?.find((value) =>
      Boolean(findCategoryById(categories, value)),
    ) ??
    undefined;

  if (ocrCategory && findCategoryById(categories, ocrCategory)) {
    return {
      categoryId: ocrCategory,
      source: "ocr",
    };
  }

  return {
    categoryId: "uncategorized",
    source: "none",
  };
}

function chooseCategoryLabel(draft: {
  description: string;
  merchant: string;
}) {
  const merchant = draft.merchant.trim();
  if (merchant) {
    return merchant;
  }

  const description = draft.description.trim();
  if (description && description !== "Receipt pending review") {
    return description;
  }

  return "";
}

function findCategoryById(categories: Category[], categoryId?: string | null) {
  if (!categoryId) {
    return null;
  }

  return categories.find((category) => category.id === categoryId) ?? null;
}

function readStringRecordValue(
  record: Record<string, unknown> | null | undefined,
  key: string,
) {
  const value = record?.[key];
  return typeof value === "string" && value.trim() ? value : null;
}

function describeCategoryRecommendation(
  recommendation: CategoryRecommendation,
  category: Category | null,
) {
  if (!category || recommendation.source === "none") {
    return null;
  }

  switch (recommendation.source) {
    case "history":
      return {
        title: "Suggested from past entries",
        body: `Apply ${category.name}${
          recommendation.matchedLabel
            ? ` based on ${recommendation.matchedLabel}`
            : ""
        }.`,
      };
    case "keyword":
      return {
        title: "Predicted from description",
        body: `Apply ${category.name} from the current merchant/description words.`,
      };
    case "core":
      return {
        title: "Suggested by desktop AI",
        body: `Apply ${category.name} from the synced categorization model.`,
      };
    case "ocr":
      return {
        title: "Suggested from OCR",
        body: `Apply ${category.name} from the detected receipt items.`,
      };
    default:
      return null;
  }
}

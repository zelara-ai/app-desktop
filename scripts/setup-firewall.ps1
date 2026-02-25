# Zelara Desktop - Windows Firewall Setup
# Run this script as Administrator to allow port 8765 for device linking

$ruleName = "Zelara Desktop - Device Linking"
$port = 8765
$protocol = "TCP"

Write-Host "Setting up Windows Firewall rule for Zelara..." -ForegroundColor Cyan
Write-Host "Rule Name: $ruleName" -ForegroundColor Yellow
Write-Host "Port: $port" -ForegroundColor Yellow
Write-Host "Protocol: $protocol" -ForegroundColor Yellow
Write-Host ""

# Check if rule already exists
$existingRule = Get-NetFirewallRule -DisplayName $ruleName -ErrorAction SilentlyContinue

if ($existingRule) {
    Write-Host "Firewall rule already exists. Updating..." -ForegroundColor Yellow
    Remove-NetFirewallRule -DisplayName $ruleName
}

# Create new firewall rule
try {
    New-NetFirewallRule `
        -DisplayName $ruleName `
        -Direction Inbound `
        -LocalPort $port `
        -Protocol $protocol `
        -Action Allow `
        -Profile Domain,Private,Public `
        -Description "Allows Zelara mobile app to connect to Desktop for device linking and task offloading"

    Write-Host ""
    Write-Host "Firewall rule created successfully!" -ForegroundColor Green
    Write-Host ""
    Write-Host "Your Desktop can now accept connections from mobile devices on port $port" -ForegroundColor Green
    Write-Host "Make sure both devices are on the same WiFi network." -ForegroundColor Cyan
} catch {
    Write-Host ""
    Write-Host "Failed to create firewall rule: $_" -ForegroundColor Red
    Write-Host ""
    Write-Host "Please make sure you are running this script as Administrator:" -ForegroundColor Yellow
    Write-Host "  1. Right-click on PowerShell" -ForegroundColor Yellow
    Write-Host "  2. Select Run as Administrator" -ForegroundColor Yellow
    Write-Host "  3. Navigate to this directory and run the script again" -ForegroundColor Yellow
    exit 1
}

Write-Host ""
Write-Host "Press any key to exit..."
$null = $Host.UI.RawUI.ReadKey("NoEcho,IncludeKeyDown")

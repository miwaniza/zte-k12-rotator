# ==============================================================================
#  ZTE K12 1-Click IP & Region Rotator & System Toast Notification
# ==============================================================================
$ErrorActionPreference = "SilentlyContinue"

$InstallDir = "$env:LOCALAPPDATA\zte-k12-rotator"
if (-not (Test-Path "$InstallDir\zte-control.exe")) {
    $InstallDir = $PSScriptRoot
}

# 1. Trigger rotation via local service API or binary
$Rotated = $false
try {
    $resp = Invoke-RestMethod -Uri "http://127.0.0.1:8080/api/reconnect" -TimeoutSec 10
    if ($resp.status -eq "success") { $Rotated = $true }
} catch {
    Start-Process "$InstallDir\zte-control.exe" -ArgumentList "reconnect" -Wait -WindowStyle Hidden
    $Rotated = $true
}

Start-Sleep -Seconds 3

# 2. Query status & new public IP + Region
$NewIp = "--"
$Region = "Київська обл."
$City = "Київ"
$Country = "UA"
$Isp = "Kyivstar"
$Band = "LTE"
$Pci = "--"
$Rsrp = "--"

try {
    $geo = Invoke-RestMethod -Uri "http://127.0.0.1:8080/api/geo" -TimeoutSec 4
    if (-not $geo.query -and -not $geo.ip) {
        $geo = Invoke-RestMethod -Uri "http://ip-api.com/json/?fields=status,country,regionName,city,isp,as,query" -TimeoutSec 4
    }

    if ($geo.query) { $NewIp = $geo.query }
    elseif ($geo.ip) { $NewIp = $geo.ip }

    if ($geo.regionName) { $Region = $geo.regionName }
    elseif ($geo.region) { $Region = $geo.region }

    if ($geo.city) { $City = $geo.city }
    if ($geo.isp) { $Isp = $geo.isp }
    elseif ($geo.connection.isp) { $Isp = $geo.connection.isp }
} catch {}

try {
    $st = Invoke-RestMethod -Uri "http://127.0.0.1:8080/goform/goform_get_cmd_process?cmd=wan_active_band,lte_pci,lte_rsrp,wan_ipaddr&multi_data=1&isTest=false" -TimeoutSec 3
    if ($st.wan_active_band) { $Band = $st.wan_active_band }
    if ($st.lte_pci) { $Pci = $st.lte_pci }
    if ($st.lte_rsrp) { $Rsrp = $st.lte_rsrp }
} catch {}

# 3. Show Native Windows Toast Notification
$toastShown = $false
try {
    [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
    [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null

    $template = @"
<toast duration="short">
    <visual>
        <binding template="ToastGeneric">
            <text>📡 ZTE K12: IP & Регіон змінено! ✅</text>
            <text>🌐 Новий IP: $NewIp</text>
            <text>📍 Регіон: $Region, $City ($Isp) | $Band</text>
        </binding>
    </visual>
</toast>
"@
    $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
    $xml.LoadXml($template)
    $toast = New-Object Windows.UI.Notifications.ToastNotification $xml
    [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier("ZTE K12 IP Rotator").Show($toast)
    $toastShown = $true
} catch {}

if (-not $toastShown) {
    try {
        Add-Type -AssemblyName System.Windows.Forms
        $notify = New-Object System.Windows.Forms.NotifyIcon
        $notify.Icon = [System.Drawing.SystemIcons]::Information
        $notify.BalloonTipTitle = "📡 ZTE K12: Новий IP отримано! ✅"
        $notify.BalloonTipText = "IP: $NewIp ($Region, $City) | $Band (PCI $Pci)"
        $notify.Visible = $true
        $notify.ShowBalloonTip(4000)
        Start-Sleep -Seconds 4
        $notify.Dispose()
    } catch {}
}

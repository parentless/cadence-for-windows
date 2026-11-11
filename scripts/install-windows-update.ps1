param(
    [Parameter(Mandatory)][int]$ProcessId,
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][string]$Staged
)
$ErrorActionPreference = 'Stop'
$backup = "$Executable.previous"
try {
    $process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if ($process -and -not $process.WaitForExit(60000)) { throw 'Sonder did not exit within 60 seconds.' }
    # Antivirus can hold an image briefly after exit.
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        try {
            Move-Item -LiteralPath $Executable -Destination $backup -Force
            break
        } catch {
            if ($attempt -eq 29) { throw }
            Start-Sleep -Milliseconds 500
        }
    }
    try { Move-Item -LiteralPath $Staged -Destination $Executable -Force }
    catch {
        Move-Item -LiteralPath $backup -Destination $Executable -Force
        throw
    }
    Start-Process -FilePath $Executable
    Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
} catch {
    Add-Type -AssemblyName PresentationFramework
    [System.Windows.MessageBox]::Show("Sonder could not install the update: $($_.Exception.Message)", 'Sonder update') | Out-Null
    if (Test-Path -LiteralPath $Executable) { Start-Process -FilePath $Executable }
} finally {
    Remove-Item -LiteralPath $Staged -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue
}

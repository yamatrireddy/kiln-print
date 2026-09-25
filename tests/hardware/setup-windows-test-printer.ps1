#Requires -RunAsAdministrator
<#
.SYNOPSIS
  Creates a v3 "Generic / Text Only" printer for byte-exact RAW spooler tests.

.DESCRIPTION
  The printer is attached to the NUL: port, so nothing is ever printed. The spooler test
  redirects the job's output to a temporary file and compares it byte for byte with the
  payload. Remove it afterwards with -Remove.

.EXAMPLE
  .\setup-windows-test-printer.ps1
  $env:KILN_TEST_RAW_PRINTER = "Kiln Test Raw"
  cargo test -p kiln-provider-windows --test spooler -- --ignored --test-threads=1

.EXAMPLE
  .\setup-windows-test-printer.ps1 -Remove
#>
param(
    [string]$PrinterName = "Kiln Test Raw",
    [switch]$Remove
)
$ErrorActionPreference = "Stop"
$driver = "Generic / Text Only"

if ($Remove) {
    if (Get-Printer -Name $PrinterName -ErrorAction SilentlyContinue) {
        Remove-Printer -Name $PrinterName
        Write-Host "Removed printer '$PrinterName'."
    }
    return
}

if (-not (Get-PrinterDriver -Name $driver -ErrorAction SilentlyContinue)) {
    Add-PrinterDriver -Name $driver
}
if (-not (Get-PrinterPort -Name "NUL:" -ErrorAction SilentlyContinue)) {
    Add-PrinterPort -Name "NUL:"
}
if (-not (Get-Printer -Name $PrinterName -ErrorAction SilentlyContinue)) {
    Add-Printer -Name $PrinterName -DriverName $driver -PortName "NUL:"
}
Write-Host "Printer '$PrinterName' is ready. Set KILN_TEST_RAW_PRINTER=`"$PrinterName`" to run the RAW spooler test."

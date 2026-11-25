; Pawl NSIS installer hooks (bundle.windows.nsis.installerHooks in tauri.conf.json).
;
; Before files are replaced (install / upgrade) or removed (uninstall), stop Pawl processes that run from THIS
; install folder: the desktop app (pawl-desktop.exe) and the hub sidecar (pawl.exe) it may have started, together
; with the agents the hub runs (taskkill /T). The hub keeps running after the app quits, so without this an upgrade
; fails with "Error opening file for writing: ...\pawl.exe".
;
; Only processes whose executable lives in $INSTDIR are touched: a `pawl serve` the user runs from anywhere else, or
; another user's or another install's Pawl, is left alone. The folder reaches PowerShell through an environment
; variable (never pasted into the command), so no install path can change the script.

!macro PAWL_STOP_RUNNING
  DetailPrint "Closing Pawl if it is running from $INSTDIR..."
  System::Call 'kernel32::SetEnvironmentVariable(t "PAWL_INSTDIR", t "$INSTDIR") i'
  nsExec::ExecToLog `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "& { $$dir = [IO.Path]::GetFullPath($$env:PAWL_INSTDIR).TrimEnd([char]92); $$names = @('pawl.exe', 'pawl-desktop.exe'); $$found = @(Get-CimInstance Win32_Process | Where-Object { ($$names -contains $$_.Name) -and $$_.ExecutablePath -and ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($$_.ExecutablePath)) -eq $$dir) }); foreach ($$p in $$found) { Write-Output ('Stopping ' + $$p.Name + ' (pid ' + $$p.ProcessId + ')'); & taskkill.exe /PID $$p.ProcessId /T /F | Out-Null }; foreach ($$p in $$found) { Wait-Process -Id $$p.ProcessId -Timeout 15 -ErrorAction SilentlyContinue } }"`
  Pop $0
  System::Call 'kernel32::SetEnvironmentVariable(t "PAWL_INSTDIR", p 0) i'
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro PAWL_STOP_RUNNING
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro PAWL_STOP_RUNNING
!macroend

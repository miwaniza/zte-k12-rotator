Set WshShell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
scriptDir = fso.GetParentFolderName(WScript.ScriptFullName)
exePath = scriptDir & "\zte-control.exe"
WshShell.Run Chr(34) & exePath & Chr(34) & " ui --no-open", 0, False

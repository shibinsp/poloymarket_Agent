Set WshShell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")

scriptDir = fso.GetParentFolderName(WScript.ScriptFullName)
agentDir = scriptDir & "\polymarket-agent"

WshShell.CurrentDirectory = agentDir
cmdLine = "cmd /c target\release\polymarket-agent.exe --mode paper >> agent.log 2>&1"
WshShell.Run cmdLine, 0, False

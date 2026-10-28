Set WshShell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")

scriptDir = fso.GetParentFolderName(WScript.ScriptFullName)
agentDir = scriptDir & "\polymarket-agent"

WshShell.CurrentDirectory = agentDir

' 1. Start Polymarket Agent in silent background
cmdLineAgent = "cmd /c target\release\polymarket-agent.exe --mode paper >> agent.log 2>&1"
WshShell.Run cmdLineAgent, 0, False

' 2. Start Cloudflare Remote Tunnel in silent background
cmdLineTunnel = "cmd /c python scratch\manage_tunnel.py start --quiet >> agent.log 2>&1"
WshShell.Run cmdLineTunnel, 0, False

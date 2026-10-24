Set WshShell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")

scriptDir = fso.GetParentFolderName(WScript.ScriptFullName)
startupFolder = WshShell.SpecialFolders("Startup")
shortcutPath = startupFolder & "\PolymarketTradingAgent.lnk"

Set shortcut = WshShell.CreateShortcut(shortcutPath)
shortcut.TargetPath = "wscript.exe"
shortcut.Arguments = """" & scriptDir & "\run_background.vbs"""
shortcut.WorkingDirectory = scriptDir & "\polymarket-agent"
shortcut.WindowStyle = 7
shortcut.Description = "Polymarket Autonomous Trading Agent"
shortcut.Save

WScript.Echo "Auto-start shortcut created successfully in:"
WScript.Echo shortcutPath

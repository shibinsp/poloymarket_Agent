Set WshShell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")

startupFolder = WshShell.SpecialFolders("Startup")
shortcutPath = startupFolder & "\PolymarketTradingAgent.lnk"

If fso.FileExists(shortcutPath) Then
    fso.DeleteFile(shortcutPath)
    WScript.Echo "Auto-start shortcut removed successfully."
Else
    WScript.Echo "No auto-start shortcut found."
End If

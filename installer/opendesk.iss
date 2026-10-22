; Script generated for OpenDesk 2.0 Official Windows Installer
#define MyAppName "OpenDesk"
#define MyAppVersion "2.2.0"
#define MyAppPublisher "OpenDesk Team"
#define MyAppURL "https://github.com/opendesk"
#define MyAppExeName "OpenDesk.exe"

[Setup]
AppId={{C78F7A41-75A2-4D7F-8A2F-9B6C3F81254D}}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
LicenseFile=
OutputDir=D:\opendesk\builds\windows
OutputBaseFilename=OpenDesk-Setup
SetupIconFile=D:\opendesk\installer\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "startmenuicon"; Description: "Buat pintasan di Menu Mulai (Start Menu)"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "D:\opendesk\builds\windows\OpenDesk.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "D:\opendesk\installer\icon.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon; IconFilename: "{app}\icon.ico"
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"
Name: "{autoprograms}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#MyAppName}"; ValueData: """{app}\{#MyAppExeName}"" --background"; Flags: uninsdeletevalue

[UninstallRun]
Filename: "{cmd}"; Parameters: "/c taskkill /F /IM wscript.exe & taskkill /F /IM {#MyAppExeName} & taskkill /F /IM OpenDesk.exe & taskkill /F /IM OpenDesk-Host.exe & taskkill /F /IM OpenDesk-Admin.exe"; Flags: runhidden
Filename: "{cmd}"; Parameters: "/c del /F /Q ""%APPDATA%\OpenDesk\watchdog.lock"" ""%APPDATA%\OpenDesk\opendesk-watchdog.vbs"" ""%LOCALAPPDATA%\Microsoft\WindowsApps\opendesk.cmd"" 2>nul"; Flags: runhidden

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

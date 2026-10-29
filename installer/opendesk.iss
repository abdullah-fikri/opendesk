; Inno Setup script for the OpenDesk Windows installer.
; Build: ISCC installer\opendesk.iss  (or `make setup`; CI passes /DMyAppVersion=<version>).
; Expects builds\windows\OpenDesk.exe, produced by `make windows`.
#define MyAppName "OpenDesk"
#ifndef MyAppVersion
  #define MyAppVersion "1.2.0"
#endif
#define MyAppPublisher "OpenDesk Team"
#define MyAppURL "https://github.com/abdullah-fikri/opendesk"
#define MyAppExeName "OpenDesk.exe"
; Must match ELEVATED_TASK_NAME in client-desktop/src/infra/autostart.rs
#define ElevatedTaskName "OpenDesk Autostart"
; Must match SERVICE_NAME in client-desktop/src/infra/system_service/win.rs
#define ServiceName "OpenDeskService"

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
OutputDir={#SourcePath}..\builds\windows
OutputBaseFilename=OpenDesk-Setup
SetupIconFile={#SourcePath}icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
; OpenDesk.exe is x64: install into the 64-bit Program Files and registry view.
; 1.0.0-1.0.2 ran in 32-bit mode (Program Files (x86)); [Code] migrates those installs.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Admin once at install: needed to register the SYSTEM service that runs the app in the
; logged-on user's session, so viewers can control the UAC prompt, the lock screen and elevated
; windows (UIPI). The "highest privileges" logon task is the fallback when the service fails.
PrivilegesRequired=admin

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
; ignoreversion and no version check anywhere in [Code]: an older installer can run over a newer install.
; The auto-update rollback depends on this (it runs the previous installer with /VERYSILENT as SYSTEM
; from the service); keep it that way. See docs/auto-update.md.
Source: "{#SourcePath}..\builds\windows\OpenDesk.exe"; DestDir: "{app}"; Flags: ignoreversion
; Still shipped: the tray (client-desktop/src/infra/tray/win.rs) loads icon.ico by relative path.
Source: "{#SourcePath}icon.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; Shortcuts use the icon embedded in the exe (client-desktop/build.rs, icon.rc).
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autoprograms}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"

; Autostart is no longer an HKCU Run value: the service (or the elevated logon task) is created in
; [Code] (HKCU Run is only written there as a fallback if neither can be created).

[UninstallRun]
; Stopping the service also ends the agent it runs.
Filename: "{sys}\sc.exe"; Parameters: "stop {#ServiceName}"; Flags: runhidden; RunOnceId: "StopService"
Filename: "{sys}\sc.exe"; Parameters: "delete {#ServiceName}"; Flags: runhidden; RunOnceId: "DeleteService"
; Delete the task first so its restart-on-failure policy cannot bring the app back after taskkill.
Filename: "{sys}\schtasks.exe"; Parameters: "/End /TN ""{#ElevatedTaskName}"""; Flags: runhidden; RunOnceId: "EndTask"
Filename: "{sys}\schtasks.exe"; Parameters: "/Delete /TN ""{#ElevatedTaskName}"" /F"; Flags: runhidden; RunOnceId: "DeleteTask"
Filename: "{cmd}"; Parameters: "/c taskkill /F /IM wscript.exe & taskkill /F /IM {#MyAppExeName} & taskkill /F /IM OpenDesk.exe & taskkill /F /IM OpenDesk-Host.exe & taskkill /F /IM OpenDesk-Admin.exe"; Flags: runhidden; RunOnceId: "KillApp"
Filename: "{cmd}"; Parameters: "/c del /F /Q ""%APPDATA%\OpenDesk\watchdog.lock"" ""%APPDATA%\OpenDesk\opendesk-watchdog.vbs"" ""%LOCALAPPDATA%\Microsoft\WindowsApps\opendesk.cmd"" 2>nul"; Flags: runhidden; RunOnceId: "DelFiles"
Filename: "{cmd}"; Parameters: "/c reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v OpenDesk /f & reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v OpenDeskWatchdog /f"; Flags: runhidden; RunOnceId: "DelRunKeys"

[Run]
; Runs as the original (non-elevated) user; the app hands off to the service (or the elevated task) itself.
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent runasoriginaluser

[Code]
const
  TaskName = '{#ElevatedTaskName}';
  ServiceName = '{#ServiceName}';
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  // Same AppId as [Setup]: its closing braces are literal (only the opening pair is an escape).
  // Used for old per-user installs (HKCU) and old 32-bit-mode installs (HKLM32).
  UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{C78F7A41-75A2-4D7F-8A2F-9B6C3F81254D}}_is1';

var
  { The old 32-bit-mode install had the desktop icon task unchecked; keep it unchecked. }
  OldNoDesktopIcon: Boolean;

{ XML-escape and turn every non-ASCII char into &#N; so the file is pure ASCII.
  schtasks rejects UTF-8 with BOM/declaration and mis-reads non-ASCII bytes otherwise. }
function XmlEscape(const S: String): String;
var
  I, C, Lo: Integer;
begin
  Result := '';
  I := 1;
  while I <= Length(S) do
  begin
    C := Ord(S[I]);
    if C = Ord('&') then
      Result := Result + '&amp;'
    else if C = Ord('<') then
      Result := Result + '&lt;'
    else if C = Ord('>') then
      Result := Result + '&gt;'
    else if C < 128 then
      Result := Result + S[I]
    else
    begin
      { Surrogate pair -> one code point (XML char refs are code points). }
      if (C >= $D800) and (C <= $DBFF) and (I < Length(S)) then
      begin
        Lo := Ord(S[I + 1]);
        if (Lo >= $DC00) and (Lo <= $DFFF) then
        begin
          C := $10000 + ((C - $D800) shl 10) + (Lo - $DC00);
          I := I + 1;
        end;
      end;
      Result := Result + '&#' + IntToStr(C) + ';';
    end;
    I := I + 1;
  end;
end;

{ Account that runs Setup (after elevation). For an admin user this is the same account
  that is logged on. If a standard user typed another admin's credentials, this is that
  admin account; see the readme notes in the task report. }
function TaskUserId: String;
var
  Domain: String;
begin
  Domain := GetEnv('USERDOMAIN');
  if Domain <> '' then
    Result := Domain + '\' + GetUserNameString
  else
    Result := GetUserNameString;
end;

function BuildTaskXml: String;
var
  User, AppDir: String;
begin
  User := XmlEscape(TaskUserId);
  AppDir := ExpandConstant('{app}');
  { Logon delay: give Explorer's notification area time to exist before the tray icon is added.
    Priority 4 = normal (Task Scheduler's default 7 would run the app below normal).
    RestartOnFailure + no time limit + battery flags replace the old VBS watchdog. }
  Result :=
    '<Task version="1.3" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">' + #13#10 +
    '  <RegistrationInfo>' + #13#10 +
    '    <Author>OpenDesk Setup</Author>' + #13#10 +
    '    <Description>Starts OpenDesk in the background at logon with administrator rights.</Description>' + #13#10 +
    '  </RegistrationInfo>' + #13#10 +
    '  <Triggers>' + #13#10 +
    '    <LogonTrigger>' + #13#10 +
    '      <Enabled>true</Enabled>' + #13#10 +
    '      <UserId>' + User + '</UserId>' + #13#10 +
    '      <Delay>PT10S</Delay>' + #13#10 +
    '    </LogonTrigger>' + #13#10 +
    '  </Triggers>' + #13#10 +
    '  <Principals>' + #13#10 +
    '    <Principal id="Author">' + #13#10 +
    '      <UserId>' + User + '</UserId>' + #13#10 +
    '      <LogonType>InteractiveToken</LogonType>' + #13#10 +
    '      <RunLevel>HighestAvailable</RunLevel>' + #13#10 +
    '    </Principal>' + #13#10 +
    '  </Principals>' + #13#10 +
    '  <Settings>' + #13#10 +
    '    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>' + #13#10 +
    '    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>' + #13#10 +
    '    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>' + #13#10 +
    '    <AllowHardTerminate>true</AllowHardTerminate>' + #13#10 +
    '    <StartWhenAvailable>false</StartWhenAvailable>' + #13#10 +
    '    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>' + #13#10 +
    '    <IdleSettings>' + #13#10 +
    '      <StopOnIdleEnd>false</StopOnIdleEnd>' + #13#10 +
    '      <RestartOnIdle>false</RestartOnIdle>' + #13#10 +
    '    </IdleSettings>' + #13#10 +
    '    <AllowStartOnDemand>true</AllowStartOnDemand>' + #13#10 +
    '    <Enabled>true</Enabled>' + #13#10 +
    '    <Hidden>false</Hidden>' + #13#10 +
    '    <RunOnlyIfIdle>false</RunOnlyIfIdle>' + #13#10 +
    '    <WakeToRun>false</WakeToRun>' + #13#10 +
    '    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>' + #13#10 +
    '    <Priority>4</Priority>' + #13#10 +
    '    <RestartOnFailure>' + #13#10 +
    '      <Interval>PT1M</Interval>' + #13#10 +
    '      <Count>3</Count>' + #13#10 +
    '    </RestartOnFailure>' + #13#10 +
    '  </Settings>' + #13#10 +
    '  <Actions Context="Author">' + #13#10 +
    '    <Exec>' + #13#10 +
    '      <Command>' + XmlEscape(AppDir + '\{#MyAppExeName}') + '</Command>' + #13#10 +
    '      <Arguments>--background</Arguments>' + #13#10 +
    '      <WorkingDirectory>' + XmlEscape(AppDir) + '</WorkingDirectory>' + #13#10 +
    '    </Exec>' + #13#10 +
    '  </Actions>' + #13#10 +
    '</Task>' + #13#10;
end;

function RunHidden(const FileName, Params: String): Integer;
begin
  if not Exec(FileName, Params, '', SW_HIDE, ewWaitUntilTerminated, Result) then
    Result := -1;
end;

function CreateElevatedTask: Boolean;
var
  XmlPath: String;
  Code: Integer;
begin
  XmlPath := ExpandConstant('{tmp}\opendesk-task.xml');
  { No BOM and no <?xml?> declaration: verified that schtasks accepts this pure-ASCII form. }
  Result := SaveStringToFile(XmlPath, BuildTaskXml, False);
  if Result then
  begin
    Code := RunHidden(ExpandConstant('{sys}\schtasks.exe'), '/Create /TN "' + TaskName + '" /XML "' + XmlPath + '" /F');
    Result := Code = 0;
    Log(Format('schtasks /Create "%s" for %s -> %d', [TaskName, TaskUserId, Code]));
  end;
end;

{ Creates (or, on upgrade, updates) the auto-start LocalSystem service and starts it. }
function InstallService: Boolean;
var
  Sc, Args: String;
  Code: Integer;
begin
  Sc := ExpandConstant('{sys}\sc.exe');
  Args := ' ' + ServiceName + ' binPath= "\"' + ExpandConstant('{app}\{#MyAppExeName}') + '\" --service" start= auto DisplayName= "OpenDesk Service"';
  Code := RunHidden(Sc, 'create' + Args);
  { 1073 = already exists (upgrade): point it at this install instead. }
  if Code = 1073 then
    Code := RunHidden(Sc, 'config' + Args);
  Log(Format('sc create/config %s -> %d', [ServiceName, Code]));
  Result := Code = 0;
  if Result then
  begin
    RunHidden(Sc, 'description ' + ServiceName + ' "Runs OpenDesk in the logged-on user''s session so the UAC prompt and the lock screen can be controlled remotely."');
    RunHidden(Sc, 'failure ' + ServiceName + ' reset= 86400 actions= restart/5000/restart/5000/restart/30000');
    Code := RunHidden(Sc, 'start ' + ServiceName);
    Log(Format('sc start %s -> %d', [ServiceName, Code]));
  end;
end;

{ Remove pre-task autostart (HKCU Run + VBS watchdog) so it never starts a second, non-elevated copy. }
procedure RemoveLegacyAutostart;
var
  AppData: String;
begin
  RegDeleteValue(HKEY_CURRENT_USER, RunKey, 'OpenDesk');
  RegDeleteValue(HKEY_CURRENT_USER, RunKey, 'OpenDeskWatchdog');
  AppData := ExpandConstant('{userappdata}\OpenDesk');
  DeleteFile(AppData + '\opendesk-watchdog.vbs');
  DeleteFile(AppData + '\watchdog.lock');
end;

{ 1.0.0-1.0.2 were installed in 32-bit mode: Program Files (x86) and the 32-bit registry view. }
function FindOldX86Install(var Dir, Uninstaller: String): Boolean;
begin
  Result := False;
  if not IsWin64 then
    Exit;
  if not RegQueryStringValue(HKLM32, UninstallKey, 'UninstallString', Uninstaller) then
    Exit;
  Uninstaller := RemoveQuotes(Uninstaller);
  if not RegQueryStringValue(HKLM32, UninstallKey, 'InstallLocation', Dir) or (Dir = '') then
    Dir := ExtractFileDir(Uninstaller);
  Dir := RemoveBackslashUnlessRoot(Dir);
  Result := True;
end;

{ unins000.exe copies itself to %TEMP% and exits at once, so Exec cannot wait for it.
  The copy deletes the original when it is done: wait for that (max 60 s). }
function RunOldUninstaller(const Uninstaller: String): Boolean;
var
  Code, Waited: Integer;
begin
  Result := True;
  if not FileExists(Uninstaller) then
    Exit;
  Code := RunHidden(Uninstaller, '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART');
  if Code = -1 then
  begin
    Log(Format('Old uninstaller could not be started: %s', [Uninstaller]));
    Exit;
  end;
  Waited := 0;
  while FileExists(Uninstaller) and (Waited < 60000) do
  begin
    Sleep(250);
    Waited := Waited + 250;
  end;
  Result := not FileExists(Uninstaller);
  if not Result then
    Log(Format('Old uninstaller still present after %d ms: %s (exit %d)', [Waited, Uninstaller, Code]))
  else
    Log(Format('Removed old install: %s (exit %d, %d ms)', [Uninstaller, Code, Waited]));
end;

procedure InitializeWizard;
var
  OldDir, OldUninstaller, OldTasks, Pf32: String;
begin
  if not FindOldX86Install(OldDir, OldUninstaller) then
    Exit;
  Log(Format('Old 32-bit-mode install found: %s', [OldDir]));

  { Read before PrepareToInstall uninstalls it (the key goes with it). }
  if RegQueryStringValue(HKLM32, UninstallKey, 'Inno Setup: Selected Tasks', OldTasks) then
    OldNoDesktopIcon := Pos(',desktopicon,', ',' + Lowercase(OldTasks) + ',') = 0;

  { An explicit /DIR= wins. Default dir -> the 64-bit Program Files; custom dir -> keep it. }
  if ExpandConstant('{param:DIR|}') <> '' then
    Exit;
  Pf32 := Uppercase(AddBackslash(ExpandConstant('{commonpf32}')));
  if Pos(Pf32, Uppercase(AddBackslash(OldDir))) = 1 then
    WizardForm.DirEdit.Text := ExpandConstant('{autopf}\{#MyAppName}')
  else
    WizardForm.DirEdit.Text := OldDir;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  { Once: the user may still tick it again on this page. }
  if (CurPageID = wpSelectTasks) and OldNoDesktopIcon then
  begin
    WizardSelectTasks('!desktopicon');
    OldNoDesktopIcon := False;
  end;
end;

{ Stop every running copy before files are replaced (upgrade). }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  AppData, OldDir, OldUninstaller: String;
begin
  Result := '';
  RunHidden(ExpandConstant('{sys}\sc.exe'), 'stop ' + ServiceName);
  RunHidden(ExpandConstant('{sys}\schtasks.exe'), '/End /TN "' + TaskName + '"');
  { sc stop returns before the service has ended its agent. }
  Sleep(1500);

  { An old watchdog sees this flag and exits within 10 s instead of respawning the app. }
  AppData := ExpandConstant('{userappdata}\OpenDesk');
  ForceDirectories(AppData);
  SaveStringToFile(AppData + '\user_exit.flag', 'stopped', False);

  RunHidden(ExpandConstant('{cmd}'), '/c taskkill /F /IM OpenDesk.exe & taskkill /F /IM OpenDesk-Host.exe & taskkill /F /IM OpenDesk-Admin.exe');

  { Older versions installed per-user (PrivilegesRequired=lowest). Its Start Menu shortcut would
    launch an old exe without the elevated hand-off -> remove that install silently. }
  if RegQueryStringValue(HKEY_CURRENT_USER, UninstallKey, 'UninstallString', OldUninstaller) then
    RunOldUninstaller(RemoveQuotes(OldUninstaller));

  { 1.0.0-1.0.2 (32-bit mode): remove it so only the 64-bit install remains. Its uninstaller also
    deletes the service and the task; CurStepChanged creates them again for the new app dir. }
  if FindOldX86Install(OldDir, OldUninstaller) then
  begin
    { No wizard page in silent mode; an explicit /TASKS= wins.
      TODO: verify on a VM that this keeps the desktop icon off in /SILENT and /VERYSILENT. }
    if OldNoDesktopIcon and WizardSilent and (ExpandConstant('{param:TASKS|}') = '') then
      WizardSelectTasks('!desktopicon');
    { Still running in the same dir: it would delete the files this setup is about to copy. }
    if not RunOldUninstaller(OldUninstaller) and
       SameText(AddBackslash(OldDir), AddBackslash(WizardDirValue)) then
    begin
      Result := 'The previous OpenDesk version could not be removed. Wait a minute and run Setup again.';
      Exit;
    end;
    { Uninstaller missing or timed out: drop the stale entry so Apps & Features lists one OpenDesk. }
    if RegKeyExists(HKLM32, UninstallKey) then
    begin
      RegDeleteKeyIncludingSubkeys(HKLM32, UninstallKey);
      Log('Deleted stale 32-bit uninstall entry');
    end;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Exe: String;
begin
  if CurStep <> ssPostInstall then
    Exit;

  { PrepareToInstall set the stop flag; the service would not start the app while it exists. }
  DeleteFile(ExpandConstant('{userappdata}\OpenDesk\user_exit.flag'));
  if InstallService then
  begin
    { The service replaces the elevated task of older installs. }
    RunHidden(ExpandConstant('{sys}\schtasks.exe'), '/Delete /TN "' + TaskName + '" /F');
    RemoveLegacyAutostart;
  end
  else if CreateElevatedTask then
  begin
    RemoveLegacyAutostart;
    { Silent upgrades skip the [Run] entry; start the elevated copy now. }
    if WizardSilent then
      RunHidden(ExpandConstant('{sys}\schtasks.exe'), '/Run /TN "' + TaskName + '"');
  end
  else
  begin
    { Fallback = old behaviour (non-elevated autostart); the app also re-adds its watchdog. }
    Exe := ExpandConstant('{app}\{#MyAppExeName}');
    RegWriteStringValue(HKEY_CURRENT_USER, RunKey, 'OpenDesk', '"' + Exe + '" --background');
    if not WizardSilent then
      MsgBox('The OpenDesk service and the elevated autostart task could not be created.' + #13#10 +
        'OpenDesk still starts automatically at logon, but without administrator rights ' +
        '(the UAC prompt, Task Manager and "Run as administrator" applications cannot be controlled remotely).',
        mbInformation, MB_OK);
  end;
end;

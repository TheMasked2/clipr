; clipr installer (Inno Setup 6.3 or newer). Built by scripts\package.ps1, which passes the version and the files:
;
;   ISCC.exe /DAppVersion=0.1.0 /DSourceDir=..\dist\clipr-0.1.0-windows-x64 /DOutputDir=..\dist installer\clipr.iss
;
; Installs for the current user (no admin prompt) into %LOCALAPPDATA%\Programs\clipr; "Install for all users"
; is offered by Inno's own dialog. Settings (%APPDATA%\clipr) and the library (%LOCALAPPDATA%\clipr) are
; separate from the program folder, so an update or reinstall keeps them. Clips are never touched.
;
; clipr's updater runs this with /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /UPDATE=1 after quitting (Inno's
; {param:UPDATE} needs the =value form): setup waits for
; clipr to finish exiting, installs, and starts clipr again (in the tray).

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef SourceDir
  #define SourceDir "..\target\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

#define AppName "clipr"
#define AppExe "clipr.exe"
#define AppUrl "https://github.com/TheMasked2/clipr"

[Setup]
; A fixed id, so every version updates the same install.
AppId={{6C3F2B1E-8A4D-4F0B-9E51-3D7A2C9B8E10}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppName}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; Windows.Graphics.Capture with a borderless capture border needs Windows 10 1903 or newer.
MinVersion=10.0.18362
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=clipr-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
LicenseFile=..\LICENSE
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
; Setup and uninstall check clipr's single-instance mutex themselves (see the code section): they ask the
; user to quit clipr from the tray first, so a recording in progress is finished properly, and an update
; waits for it.
CloseApplications=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; After an update clipr comes back the way it was: in the tray, without opening a browser tab.
Filename: "{app}\{#AppExe}"; Parameters: "--background"; Flags: nowait; Check: IsUpdate


[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  { clipr creates Local\clipr-single-instance; an unprefixed name is the same session-local mutex. }
  Mutex = 'clipr-single-instance';

function IsUpdate(): Boolean;
begin
  Result := ExpandConstant('{param:UPDATE|0}') <> '0';
end;

{ Asks the user to quit clipr until it's gone (or they cancel). }
function ClipRStopped(Action: String): Boolean;
begin
  Result := True;
  while CheckForMutexes(Mutex) do
    if SuppressibleMsgBox('clipr is running. Quit it first (right-click its tray icon > Quit clipr), so a ' +
        'recording in progress is saved properly, then click Retry to ' + Action + '.',
        mbError, MB_RETRYCANCEL, IDCANCEL) = IDCANCEL then
    begin
      Result := False;
      Exit;
    end;
end;

function InitializeSetup(): Boolean;
var
  Waited: Integer;
begin
  if IsUpdate() then
  begin
    { The updater starts setup and then quits; give clipr time to finish saving and exit. }
    Waited := 0;
    while CheckForMutexes(Mutex) and (Waited < 60000) do
    begin
      Sleep(500);
      Waited := Waited + 500;
    end;
    Result := not CheckForMutexes(Mutex);
    if not Result then
      Log('clipr is still running after 60 s; update cancelled');
    Exit;
  end;
  Result := ClipRStopped('install');
  if not Result then Exit;
  { Media Foundation encodes the video. Windows "N" editions ship without it until the Media Feature Pack is
    installed, and clipr can't record there. }
  if not FileExists(ExpandConstant('{sys}\mfplat.dll')) then
    Result := SuppressibleMsgBox(
      'This copy of Windows has no Media Foundation, which clipr needs to record video. ' +
      'On Windows N editions, install the Media Feature Pack (Settings > Apps > Optional features) first.' + #13#10#13#10 +
      'Install clipr anyway?', mbConfirmation, MB_YESNO, IDYES) = IDYES;
end;

function InitializeUninstall(): Boolean;
begin
  Result := ClipRStopped('uninstall');
end;

{ Deletes clipr's own files in Dir, then Dir if that left it empty. Only names clipr writes: anything else
  there (a clips folder someone put inside it, say) stays. }
procedure RemoveOwnFiles(Dir: String; Files, Dirs: array of String);
var
  I: Integer;
begin
  for I := 0 to GetArrayLength(Files) - 1 do
    DeleteFile(AddBackslash(Dir) + Files[I]);
  for I := 0 to GetArrayLength(Dirs) - 1 do
    DelTree(AddBackslash(Dir) + Dirs[I], True, True, True);
  RemoveDir(Dir);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Settings, Data: String;
begin
  { An install for all users runs its uninstaller as an administrator, whose profile may not be the one that
    used clipr: then the per-user files and the Run value aren't ours to touch. }
  if (CurUninstallStep <> usPostUninstall) or IsAdminInstallMode() then Exit;
  { "Start with Windows" is a Run value clipr writes itself; it would point at a removed exe. }
  RegDeleteValue(HKEY_CURRENT_USER, RunKey, '{#AppName}');
  Settings := ExpandConstant('{userappdata}\{#AppName}');
  Data := ExpandConstant('{localappdata}\{#AppName}');
  if (DirExists(Settings) or DirExists(Data)) and
     (SuppressibleMsgBox(
       'Also remove clipr''s settings, logs and library index?' + #13#10#13#10 +
       'Your clips are not removed: they stay in your clips folder either way.',
       mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES) then
  begin
    RemoveOwnFiles(Settings, ['config.toml', 'config.toml.tmp', 'config.toml.broken'], []);
    { No line here may start with '[': Inno reads that as a section header, even in code. }
    RemoveOwnFiles(Data, ['library.db', 'library.db-wal', 'library.db-shm', 'games.json', 'games.json.tmp',
      'clipr.port', 'discord-queue.json', 'discord-queue.json.tmp'], ['logs', 'thumbs', 'discord-tmp', 'update']);
  end;
end;

; clipr installer (Inno Setup 6). Built by scripts\package.ps1, which passes the version and the files:
;
;   ISCC.exe /DAppVersion=0.1.0 /DSourceDir=..\dist\clipr-0.1.0-windows-x64 /DOutputDir=..\dist installer\clipr.iss
;
; Installs for the current user (no admin prompt) into %LOCALAPPDATA%\Programs\clipr; "Install for all users"
; is offered by Inno's own dialog. Settings (%APPDATA%\clipr) and the library (%LOCALAPPDATA%\clipr) are
; separate from the program folder, so an update or reinstall keeps them. Clips are never touched.

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
LicenseFile={#SourceDir}\LICENSE
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
; clipr holds this mutex while it runs: setup and uninstall ask the user to quit it from the tray first, so
; a recording in progress is finished properly instead of being cut off.
AppMutex=clipr-single-instance
CloseApplications=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';

{ Media Foundation encodes the video. Windows "N" editions ship without it until the Media Feature Pack is
  installed, and clipr can't record there. }
function InitializeSetup(): Boolean;
begin
  Result := True;
  if not FileExists(ExpandConstant('{sys}\mfplat.dll')) then
    Result := SuppressibleMsgBox(
      'This copy of Windows has no Media Foundation, which clipr needs to record video. ' +
      'On Windows N editions, install the Media Feature Pack (Settings > Apps > Optional features) first.' + #13#10#13#10 +
      'Install clipr anyway?', mbConfirmation, MB_YESNO, IDYES) = IDYES;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Settings, Data: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    { "Start with Windows" is a Run value clipr writes itself; it would point at a removed exe. }
    RegDeleteValue(HKEY_CURRENT_USER, RunKey, '{#AppName}');
    Settings := ExpandConstant('{userappdata}\{#AppName}');
    Data := ExpandConstant('{localappdata}\{#AppName}');
    if (DirExists(Settings) or DirExists(Data)) and
       (SuppressibleMsgBox(
         'Also remove clipr''s settings, logs and library index?' + #13#10#13#10 +
         'Your clips are never removed: they stay in your clips folder either way.',
         mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES) then
    begin
      DelTree(Settings, True, True, True);
      DelTree(Data, True, True, True);
    end;
  end;
end;

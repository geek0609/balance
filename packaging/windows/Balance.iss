#ifndef Version
  #error "Pass the release version with /DVersion"
#endif
#ifndef Build
  #error "Pass the update build number with /DBuild"
#endif
#ifndef PackageDir
  #error "Pass the staged Windows payload with /DPackageDir"
#endif

[Setup]
AppId={{99AED542-DB17-4E54-939F-7655C1B1B1C6}
AppName=Balance
AppVersion={#Version}
AppPublisher=Balance
AppPublisherURL=https://github.com/geek0609/balance
AppUpdatesURL=https://github.com/geek0609/balance/releases
DefaultDirName={localappdata}\Programs\Balance
DefaultGroupName=Balance
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
CloseApplications=yes
CloseApplicationsFilter=balance.exe,WinSparkle.dll
RestartApplications=no
UninstallDisplayIcon={app}\balance.exe
OutputBaseFilename=balance-{#Version}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
SetupLogging=yes

[Files]
Source: "{#PackageDir}\balance.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\WinSparkle.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Balance"; Filename: "{app}\balance.exe"; WorkingDir: "{app}"

[Registry]
Root: HKCU; Subkey: "Software\Balance\Installed"; ValueType: dword; ValueName: "Build"; ValueData: "{#Build}"; Flags: uninsdeletevalue uninsdeletekeyifempty
Root: HKCU; Subkey: "Software\Balance\Updates"; ValueType: string; ValueName: "CheckForUpdates"; ValueData: "1"; Flags: createvalueifdoesntexist

[Run]
Filename: "{app}\balance.exe"; Description: "Open Balance"; Flags: nowait postinstall skipifsilent

[Code]
function InitializeSetup(): Boolean;
var
  InstalledBuild: Cardinal;
begin
  Result := True;
  if RegQueryDWordValue(HKCU, 'Software\Balance\Installed', 'Build', InstalledBuild) then
    if InstalledBuild > {#Build} then
    begin
      SuppressibleMsgBox('A newer version of Balance is already installed.', mbError, MB_OK, IDOK);
      Result := False;
    end;
end;

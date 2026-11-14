#define AppName "Clipy-W"
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{39ED5216-5B05-49BC-812E-C8C294CE1017}}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Clipy-W
DefaultDirName={localappdata}\Programs\Clipy-W
DefaultGroupName=Clipy-W
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
OutputDir=..\target\installer
OutputBaseFilename=Clipy-W-Setup-{#AppVersion}-x64
UninstallDisplayName=Clipy-W
UninstallDisplayIcon={app}\clipy-w.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
SetupLogging=yes

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "..\target\x86_64-pc-windows-msvc\release\clipy-w.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Clipy-W"; Filename: "{app}\clipy-w.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\Clipy-W"; Filename: "{app}\clipy-w.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\clipy-w.exe"; Description: "Launch Clipy-W"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\reg.exe"; Parameters: "delete ""HKCU\Software\Microsoft\Windows\CurrentVersion\Run"" /v ""Clipy-W"" /f"; Flags: runhidden

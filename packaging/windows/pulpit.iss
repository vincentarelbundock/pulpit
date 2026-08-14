; Inno Setup script for the per-user Pulpit installer.
;
; Per-user by design (SPEC-package.md §6.1): PrivilegesRequired=lowest means an
; ordinary install writes under %LOCALAPPDATA% and raises no UAC prompt. That
; matters more than it looks — a UAC prompt on an unsigned installer is the
; loudest possible "this might be malware", and avoiding the prompt entirely is
; free, whereas avoiding the signature warning costs a certificate.
;
; What this produces is exactly what §8 asks of a supported platform: an icon,
; a Start Menu entry, the native library beside the executable, and an
; Add/Remove Programs entry that uninstalls cleanly.
;
; AppId is a stable GUID and must never change: it is what lets an upgrade
; replace an install rather than sit beside it.

#define AppName "Pulpit"
#define AppExe "pulpit.exe"
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist\pulpit-windows"
#endif

[Setup]
AppId={{8B0E4F2A-3C71-4D5E-9A6B-2F8C1D7E4B93}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Vincent Arel-Bundock
AppPublisherURL=https://github.com/vincentarelbundock/pulpit
AppSupportURL=https://github.com/vincentarelbundock/pulpit/issues
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Per-user: no UAC, no elevation, no admin account required.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=..\..\dist
OutputBaseFilename=pulpit-{#AppVersion}-windows-x64-setup
SetupIconFile=..\pulpit.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName} {#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; The adapters are x64 and arm64; arm64 runs the x64 build under emulation.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\..\LICENSES\LICENSE-MIT

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; \
  GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
; pdfium.dll sits beside the executable, which is where the PDFium search
; order looks first after the environment variable. No wrapper, no PATH entry.
Source: "{#SourceDir}\pdfium.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\licenses\*"; DestDir: "{app}\licenses"; \
  Flags: ignoreversion recursesubdirs
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Offer Pulpit in "Open with" for PDFs without claiming the default handler:
; a presenter is not a general-purpose PDF reader and should not displace one.
Root: HKCU; Subkey: "Software\Classes\Applications\{#AppExe}\shell\open\command"; \
  ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; \
  Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\.pdf\OpenWithList\{#AppExe}"; \
  ValueType: string; ValueName: ""; ValueData: ""; Flags: uninsdeletekey

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch {#AppName}"; \
  Flags: nowait postinstall skipifsilent

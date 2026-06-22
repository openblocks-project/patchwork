; ============================================================
;  PatchWork installer script (standalone NSIS)
;  Build with:  makensis installer.nsi
;  Produces:    PatchWork-0.0.8-setup.exe
; ============================================================

!define APPNAME       "PatchWork"
!define COMPANYNAME   "OpenBlocks"
!define VERSION       "0.0.10"
!define EXENAME       "patchwork.exe"
!define INSTALLDIRBASE "$LOCALAPPDATA\${APPNAME}"

; ----- Modern UI -----
!include "MUI2.nsh"

Name "${APPNAME} ${VERSION}"
OutFile "PatchWork-${VERSION}-setup.exe"

; Per-user install (no admin prompt). Matches your cargo-packager
; "currentUser" intent.
RequestExecutionLevel user
InstallDir "${INSTALLDIRBASE}"

; Where the uninstaller registers itself (per-user hive).
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}"

; ----- Installer pages -----
!define MUI_ICON "assets\icons\icon.ico"
!define MUI_UNICON "assets\icons\icon.ico"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXENAME}"
!insertmacro MUI_PAGE_FINISH

; ----- Uninstaller pages -----
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; ============================================================
;  Install
; ============================================================
Section "Install"
    SetOutPath "$INSTDIR"

    ; Main executable (built by `cargo build --release`)
    File "target\release\${EXENAME}"

    ; --- Bundle any runtime assets the app needs at startup ---
    ; If PatchWork loads files from an `assets` folder at runtime,
    ; uncomment the next line so they ship with the install:
    ; File /r "assets"

    ; Start Menu shortcut
    CreateDirectory "$SMPROGRAMS\${APPNAME}"
    CreateShortcut "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\${EXENAME}"

    ; Desktop shortcut
    CreateShortcut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\${EXENAME}"

    ; Write uninstaller
    WriteUninstaller "$INSTDIR\uninstall.exe"

    ; Register in "Apps & features" so users can uninstall normally
    WriteRegStr   HKCU "${UNINSTKEY}" "DisplayName"     "${APPNAME}"
    WriteRegStr   HKCU "${UNINSTKEY}" "DisplayVersion"  "${VERSION}"
    WriteRegStr   HKCU "${UNINSTKEY}" "Publisher"       "${COMPANYNAME}"
    WriteRegStr   HKCU "${UNINSTKEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
    WriteRegStr   HKCU "${UNINSTKEY}" "InstallLocation" "$\"$INSTDIR$\""
    WriteRegStr   HKCU "${UNINSTKEY}" "DisplayIcon"     "$\"$INSTDIR\${EXENAME}$\""
    WriteRegDWORD HKCU "${UNINSTKEY}" "NoModify" 1
    WriteRegDWORD HKCU "${UNINSTKEY}" "NoRepair" 1
SectionEnd

; ============================================================
;  Uninstall
; ============================================================
Section "Uninstall"
    Delete "$INSTDIR\${EXENAME}"
    Delete "$INSTDIR\uninstall.exe"

    ; If you bundled the assets folder above, also remove it:
    ; RMDir /r "$INSTDIR\assets"

    Delete "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk"
    RMDir  "$SMPROGRAMS\${APPNAME}"
    Delete "$DESKTOP\${APPNAME}.lnk"

    RMDir "$INSTDIR"

    DeleteRegKey HKCU "${UNINSTKEY}"
SectionEnd

# Windows installer (MSI)

`mythbrowse.wxs` builds an unsigned per-machine MSI with WiX Toolset v5 (`dotnet tool install
--global wix --version 5.0.2`):

```
wix build packaging/windows/mythbrowse.wxs -arch x64 -d Version=<x.y.z from Cargo.toml> -d SourceDir=<folder with the zip's files> -o MythBrowse-<version>-x64.msi
```

- Installs to `C:\Program Files\JustMyth\MythBrowse`, adds a Start-menu shortcut and an
  Apps & features entry (uninstall).
- UpgradeCode `3CC07836-136B-4134-AC80-AB098C76D863` is fixed: never change it, or new MSIs stop
  replacing old installs. MajorUpgrade allows same-version reinstalls (CI builds share a version).
- `ci/msi-smoke-windows.ps1`: quiet install, launch check, quiet uninstall.
- Signing (Authenticode) is not done yet.

# MSIX (Microsoft Store / sideload)

`msix/AppxManifest.xml` (desktop full-trust app: `Windows.FullTrustApplication`, `runFullTrust`,
`internetClient`, Windows.Desktop ≥ 10.0.17763.0) + `msix/Assets/` (Start-menu / tile logos drawn
from the Myth mark: 44, 50, 71, 150, 310×150, 310 px) are packed by `ci/msix-windows.ps1` with the
Windows SDK's `makeappx.exe`.

- **Package identity lives in one place: `msix/identity.json`** (Name, Publisher, DisplayName,
  PublisherDisplayName). Placeholders `JustMyth.MythBrowse` / `CN=JustMyth-Dev` until the app is
  registered in Partner Center; then paste the values from Product identity there. Version is
  Cargo.toml's `a.b.c` as `a.b.c.0`.
- Outputs: the unsigned `.msix` (for Partner Center; the Store signs it), a copy signed with a
  throwaway self-signed cert made in CI (subject = Publisher) and its `.cer` for sideload tests
  (import the .cer into Local Machine > Trusted People, then open the -testsigned.msix).
- Under MSIX, writes to `%LOCALAPPDATA%\MythBrowse` (the profile) are redirected to
  `%LOCALAPPDATA%\Packages\<family>\LocalCache\Local\MythBrowse` and removed on uninstall.

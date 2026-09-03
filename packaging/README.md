# Packaging and releases

How a tag becomes installers, and how those installers reach package
managers. Everything here is driven by two workflows and one script.

## Cutting a release

1. Bump `version` in `package.json`, `src-tauri/Cargo.toml`, and
   `src-tauri/tauri.conf.json`; move the CHANGELOG's *Unreleased* section
   under the new version.
2. Tag it: `git tag v0.2.0 && git push origin v0.2.0`.
3. The **Release** workflow re-runs the checks on the tagged commit, builds
   every platform, and creates a **draft** release with the installers
   attached. Nothing is published automatically.
4. Review the draft on GitHub and click **Publish**.
5. Publishing triggers the **Packaging** workflow (below).

## Homebrew

This repository is itself a Homebrew tap: `Casks/mcpanel.rb` is the cask.
Users run

```bash
brew tap q01p/mcpanel https://github.com/Q01P/MCPanel
brew install --cask q01p/mcpanel/mcpanel
```

and `brew upgrade` follows new versions. The cask pins the release version
and the sha256 of each architecture's DMG, so it must be updated for every
release — that is the Packaging workflow's job.

Until releases are notarized, the cask clears the quarantine attribute in
`postflight` so Gatekeeper does not report the app as damaged. Remove that
block once notarization is on.

## winget

`packaging/winget/manifests/…` holds the manifests for the current release
in the layout the [community repository](https://github.com/microsoft/winget-pkgs)
expects. winget does not read from this repository; each release must be
submitted:

```bash
# from the winget-pkgs fork, on a fresh branch
cp -r packaging/winget/manifests/q/Q01P/MCPanel/<version> \
      manifests/q/Q01P/MCPanel/
winget validate manifests/q/Q01P/MCPanel/<version>   # on Windows
```

Then open a pull request against `microsoft/winget-pkgs`. Tools like
`wingetcreate` or `komac` can do the same from the release URL; the files
here are the reviewed source of truth either way. Once the first version is
accepted, `winget install Q01P.MCPanel` works, and winget installs the MSI
silently — SmartScreen is not involved, so an unsigned installer is still a
one-command install.

The installer manifest carries the MSI's **ProductCode**, which winget uses
to match the installed package for upgrades and uninstalls. Tauri's WiX
bundler generates a fresh ProductCode per version; the script reads it out
of the MSI rather than guessing.

## The Packaging workflow

`.github/workflows/packaging.yml` runs when a release is published (or on
demand with a tag). It runs

```bash
node scripts/update-packaging.mjs vX.Y.Z
```

which reads the release from the GitHub API, takes each asset's sha256 from
the release's own digest field, downloads the MSI to verify that digest and
read the ProductCode with `msiinfo` (msitools), and rewrites the cask and
the winget manifests. If anything changed it opens a pull request. Merging
that PR is what updates the tap.

The script is safe to run locally, and `--check` exits non-zero if the
committed packaging is stale for a tag:

```bash
node scripts/update-packaging.mjs v0.1.0 --check
node scripts/update-packaging.mjs v0.1.0 --product-code '{…}'   # skip the MSI download
node scripts/update-packaging.mjs v0.1.0 --release-json rel.json # offline / testing
```

## Code signing and notarization (macOS)

The Release workflow signs and notarizes the macOS builds as soon as these
repository secrets exist; with none of them set, it builds unsigned exactly
as before. Nothing else needs to change.

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | Base64 of the *Developer ID Application* `.p12` export: `base64 -i cert.p12 \| pbcopy` |
| `APPLE_CERTIFICATE_PASSWORD` | The password chosen when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | The certificate's common name, e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | The Apple ID that owns the developer account |
| `APPLE_PASSWORD` | An app-specific password for that Apple ID (not the account password) |
| `APPLE_TEAM_ID` | The 10-character team identifier |

The first three enable signing; all six enable notarization, which is what
removes the "damaged" dialog for DMG downloads. A Developer ID certificate
requires a paid Apple Developer membership.

Once releases are notarized, delete the `postflight` block from the cask.

## Windows signing

Not wired up yet. Options, in rough order of practicality for a small
project: Azure Trusted Signing (pay-as-you-go, no hardware token), or an
OV/EV certificate from a CA. Tauri supports both through
`bundle.windows` in `tauri.conf.json`. Until then, winget remains the
path that avoids SmartScreen entirely.

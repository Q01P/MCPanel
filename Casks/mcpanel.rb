# Homebrew cask for MCPanel.
#
# This repository doubles as a tap, so no separate homebrew-* repo is needed:
#
#   brew tap q01p/mcpanel https://github.com/Q01P/MCPanel
#   brew install --cask q01p/mcpanel/mcpanel
#
# The version and checksums below are rewritten by scripts/update-packaging.mjs
# on every release; edit that script, not these values, to change how they
# are derived. Checksums come from the GitHub release's own asset digests.
cask "mcpanel" do
  arch arm: "aarch64", intel: "x64"

  version "0.1.0"
  sha256 arm:   "4fb724a4b456bde1651fcfb005120bfbeea4b3f803ab6d3e650d0bcb39fb93cc",
         intel: "7d76cc5c09d860af48f6c545969c90efe67df8421db585352438a8b5724feaf1"

  url "https://github.com/Q01P/MCPanel/releases/download/v#{version}/MCPanel_#{version}_#{arch}.dmg"
  name "MCPanel"
  desc "Control panel for local MCP servers"
  homepage "https://github.com/Q01P/MCPanel"

  livecheck do
    url :url
    strategy :github_latest
  end

  app "MCPanel.app"

  # Releases are not yet notarized, so Gatekeeper would otherwise report the
  # app as "damaged". Clearing the quarantine attribute at install time is
  # what makes `brew install` a one-step path where the DMG is not. Remove
  # this block once releases carry a notarization ticket.
  postflight do
    system_command "/usr/bin/xattr",
                   args: ["-dr", "com.apple.quarantine", "#{appdir}/MCPanel.app"],
                   sudo: false
  end

  # Server config lives under the bundle identifier; credentials live in the
  # login keychain under the "mcpanel" service and are deliberately left
  # alone — `zap` should never delete secrets.
  zap trash: [
    "~/Library/Application Support/com.xseth.mcpanel",
    "~/Library/Caches/com.xseth.mcpanel",
    "~/Library/WebKit/com.xseth.mcpanel",
    "~/Library/Saved Application State/com.xseth.mcpanel.savedState",
  ]
end

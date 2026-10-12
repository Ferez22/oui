#!/usr/bin/env bash
# Print the Homebrew formula for a release.
# Usage: scripts/render-formula.sh <version> <dir with oui-<target>.tar.gz.sha256 files>
set -euo pipefail
version=$1
dir=$2
sha() { tr -d ' \n' < "$dir/oui-$1.tar.gz.sha256"; }
url() { echo "https://github.com/Ferez22/oui/releases/download/v${version}/oui-$1.tar.gz"; }

cat <<EOF
class Oui < Formula
  desc "Terminal workspace with per-profile isolated logins and tmux-style sessions"
  homepage "https://github.com/Ferez22/oui"
  version "${version}"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "$(url aarch64-apple-darwin)"
      sha256 "$(sha aarch64-apple-darwin)"
    end
    on_intel do
      url "$(url x86_64-apple-darwin)"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end

  on_linux do
    on_arm do
      url "$(url aarch64-unknown-linux-gnu)"
      sha256 "$(sha aarch64-unknown-linux-gnu)"
    end
    on_intel do
      url "$(url x86_64-unknown-linux-gnu)"
      sha256 "$(sha x86_64-unknown-linux-gnu)"
    end
  end

  def install
    bin.install "oui"
  end

  def caveats
    <<~EOS
      After upgrading, restart the background server so it runs the new version:
        oui kill-server   # closes running oUI sessions
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/oui --version")
  end
end
EOF

# typed: false
# frozen_string_literal: true

# Homebrew formula for darn — Directory-based Automerge Replication Node
#
# Install:
#   brew tap inkandswitch/darn https://github.com/inkandswitch/darn
#   brew install darn
class Darn < Formula
  desc "CLI for CRDT-backed file sync with automatic conflict resolution"
  homepage "https://github.com/inkandswitch/darn"
  version "0.5.1"
  license any_of: ["Apache-2.0", "MIT"]

  on_macos do
    url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-macos-aarch64-#{version}"
    sha256 "3455c5c64aa8e48297e7beb67b8f70418ad59b8db231f469f5a16d775b1d229e"
  end

  on_linux do
    on_arm do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-linux-aarch64-musl-#{version}"
      sha256 "1516054a72a8ba2e11636d08ba0bed91c41db404530615850ae1a0dc458777af"
    end

    on_intel do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-linux-x86_64-musl-#{version}"
      sha256 "5dcab30c099a6ebbc025377ba72e5178ab4b8f829fe3d60d507c35718acdc40f"
    end
  end

  def install
    bin.install Dir["darn-*"].first => "darn"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/darn --version")

    mkdir "test-workspace" do
      system bin/"darn", "--porcelain", "init"
      assert_predicate Pathname.pwd/".darn", :file?
    end
  end
end

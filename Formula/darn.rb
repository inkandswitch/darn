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
  version "0.4.0"
  license any_of: ["Apache-2.0", "MIT"]

  on_macos do
    url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-macos-aarch64-#{version}"
    sha256 "ef90c82c3440689f8b75682a5c7116d7194629d08e229d35664a2ea91644361f"
  end

  on_linux do
    on_arm do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-linux-aarch64-musl-#{version}"
      sha256 "7fb4d57f51cf0427e8845df3b6b97f904f4be1a94c62443c46d23ecec40358c2"
    end

    on_intel do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-linux-x86_64-musl-#{version}"
      sha256 "8e873bbe26ca9b6ca08602381e52e7d4dbeb6d56da16c79341fefa1c0932c628"
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

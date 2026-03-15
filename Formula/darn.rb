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
  version "0.6.0"
  license any_of: ["Apache-2.0", "MIT"]

  on_macos do
    url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-macos-aarch64-#{version}"
    sha256 "ff4d66239775ff7bbcc57a38165452635a190637bb0a12498dfb83798cf4b3bc"
  end

  on_linux do
    on_arm do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-linux-aarch64-musl-#{version}"
      sha256 "285c8bfb2501a5589b089d600503235abcbb5159071c974970093036a68cabac"
    end

    on_intel do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-linux-x86_64-musl-#{version}"
      sha256 "fe25131758508da3800d58856657ac0cb015899672457ce5f496e6daef0a94db"
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

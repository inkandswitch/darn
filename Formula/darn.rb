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
  version "0.6.2"
  license any_of: ["Apache-2.0", "MIT"]

  nightly_tag = "v#{version}-nightly.2026-03-30"

  on_macos do
    url "https://github.com/inkandswitch/darn/releases/download/#{nightly_tag}/darn-macos-aarch64-#{version}"
    sha256 "b0c71d449349f3c3a4312a056aa1a69f54660a18089a2e738f8750b5d19b2d4e"
  end

  on_linux do
    on_arm do
      url "https://github.com/inkandswitch/darn/releases/download/#{nightly_tag}/darn-linux-aarch64-musl-#{version}"
      sha256 "9ccdf7a6bf14845327ca64b4417e05d05798c12829f3afed63c55bcf4a046ea6"
    end

    on_intel do
      url "https://github.com/inkandswitch/darn/releases/download/#{nightly_tag}/darn-linux-x86_64-musl-#{version}"
      sha256 "87cc6ed9f1715c3a7a57b54943e5c59929e23b8c6f493909dbc80f9371bd824e"
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

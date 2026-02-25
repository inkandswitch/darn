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
  version "0.3.1"
  license any_of: ["Apache-2.0", "MIT"]

  on_macos do
    on_arm do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-macos-aarch64-#{version}"
      sha256 "055a7f480ccf1c12f9cd49b8eeb77e968ae52066782fc1ea13ab6f40f2805431"
    end

    on_intel do
      # No prebuilt Intel binary; build from source
      url "https://github.com/inkandswitch/darn/archive/refs/tags/v#{version}.tar.gz"
      # sha256 will be filled when source tarball is available
      depends_on "rust" => :build
      depends_on "openssl@3"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-linux-aarch64-musl-#{version}"
      sha256 "79ba8ced2a4ca5568dcd83cfb7223097c04cdddfb1bac16b5ee7da30317701df"
    end

    on_intel do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-linux-x86_64-musl-#{version}"
      sha256 "aa24cb11ba1ccef3624cb63959de81d2d1cc316981cf0a19cee89ebc62d3b004"
    end
  end

  def install
    if build.head? || (OS.mac? && Hardware::CPU.intel?)
      system "cargo", "install", *std_cargo_args(path: "darn_cli")
    else
      # Prebuilt binary — just copy it
      bin.install Dir["darn-*"].first => "darn"
    end
  end

  test do
    # Verify the binary runs and prints version
    assert_match version.to_s, shell_output("#{bin}/darn --version")

    # Verify init creates a .darn directory
    mkdir "test-workspace" do
      system bin/"darn", "init"
      assert_predicate Pathname.pwd/".darn", :directory?
      assert_predicate Pathname.pwd/".darn"/"manifest.json", :file?
    end
  end
end

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
      sha256 "bd75e4b10fe6a294311fa23a03a3ef4095ceb49ae9b4826cbba33a9d92f07072"
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
      sha256 "df7514b4e0704762713571f7fe68680f0259254d9ac39b6a843c573e97501277"
    end

    on_intel do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-linux-x86_64-musl-#{version}"
      sha256 "0fff6bfebea9e7ace20ae3a6cf6e964fc6f724f91b56765f041d9ef725769164"
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

    # Verify init creates a .darn marker file (porcelain mode avoids TTY requirement)
    mkdir "test-workspace" do
      system bin/"darn", "--porcelain", "init"
      assert_predicate Pathname.pwd/".darn", :file?
    end
  end
end

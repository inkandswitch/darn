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
  version "0.6.1"
  license any_of: ["Apache-2.0", "MIT"]

  on_macos do
    url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-macos-aarch64-#{version}"
    sha256 "33e1c57a85f46373bae183b254c2244e7fcecfeece2688707bdaf34e839367da"
  end

  on_linux do
    on_arm do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-linux-aarch64-musl-#{version}"
      sha256 "4e3fadfedd8d5d5ae2542d771a067fba6ea433476bfb33790952f8653e205f72"
    end

    on_intel do
      url "https://github.com/inkandswitch/darn/releases/download/v#{version}/darn-linux-x86_64-musl-#{version}"
      sha256 "bc4844352044f317e5507e1f7d8752ea4b33dea92cc7534b8282910b4b3543a5"
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

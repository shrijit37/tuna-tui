# Tuna TUI — Homebrew formula.
#
# This file documents the shape cargo-dist generates into the tap repo
# (shrijit37/homebrew-tap) on every release: class, per-OS url blocks and
# BINARY_ALIASES come from dist's template; `depends_on "yt-dlp"` and
# `depends_on "ffmpeg"` come from [dist.dependencies.homebrew] with
# stage=["run"] in dist-workspace.toml (the app spawns both binaries at
# runtime — search/resolve and stream decode).
#
# The committed copy in the tap is authoritative; dist fills the per-URL
# `sha256` values at release time, so the placeholders below are not
# installable until then.
class TunaTui < Formula
  desc "A lean, beautiful terminal music player"
  homepage "https://github.com/shrijit37/tuna-tui"
  version "0.4.0"
  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/shrijit37/tuna-tui/releases/download/v0.4.0/tuna-tui-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    if Hardware::CPU.intel?
      url "https://github.com/shrijit37/tuna-tui/releases/download/v0.4.0/tuna-tui-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end
  if OS.linux?
    if Hardware::CPU.intel?
      url "https://github.com/shrijit37/tuna-tui/releases/download/v0.4.0/tuna-tui-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end
  license "MIT"

  # Runtime deps — the app shells out to both; a formula without them
  # installs a binary that cannot play anything.
  depends_on "yt-dlp"
  depends_on "ffmpeg"

  BINARY_ALIASES = {
    "aarch64-apple-darwin": {},
    "x86_64-apple-darwin": {},
    "x86_64-unknown-linux-gnu": {}
  }

  def target_triple
    cpu = Hardware::CPU.arm? ? "aarch64" : "x86_64"
    os = OS.mac? ? "apple-darwin" : "unknown-linux-gnu"

    "#{cpu}-#{os}"
  end

  def install_binary_aliases!
    BINARY_ALIASES[target_triple.to_sym].each do |source, dests|
      dests.each do |dest|
        bin.install_symlink bin/source.to_s => dest
      end
    end
  end

  def install
    bin.install "tuna-tui"
    install_binary_aliases!
  end
end

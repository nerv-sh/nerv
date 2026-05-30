class Nerv < Formula
  desc "Inline shell completion for macOS + zsh — Fig port, no Electron, no AI"
  homepage "https://nerv.sh"
  version "0.0.0"
  license "Apache-2.0"

  depends_on :macos
  depends_on arch: :arm64

  on_macos do
    url "https://github.com/nerv-sh/nerv/releases/download/v#{version}/nerv-#{version}-aarch64-apple-darwin.tar.gz"
    sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  end

  def install
    bin.install "nerv"
    bin.install "nervd"
    # nerv-pty is the M1 opt-in figterm-style PTY shim. Idle
    # unless the user sets NERV_PTY=1 before launching the shell.
    bin.install "nerv-pty" if File.exist?("nerv-pty")
  end

  def caveats
    <<~EOS
      Add the following to your ~/.zshrc to enable inline completion:

        eval "$(nerv init zsh)"

      Then either restart your shell or run `nerv start` to spawn the daemon.
      Run `nerv doctor` at any time to verify the install.

      To remove every trace of nerv: `nerv uninstall`.
    EOS
  end

  service do
    run [opt_bin/"nervd"]
    keep_alive true
    log_path var/"log/nerv/nervd.log"
    error_log_path var/"log/nerv/nervd.log"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/nerv --version")
  end
end

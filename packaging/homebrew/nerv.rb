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
    # 700+ completion specs (gzipped JSON + schema manifest). The engine
    # reads them from share/nerv/specs (relative to the binary) whenever
    # the user spec cache is empty — no post-install spec step needed.
    pkgshare.install "specs" if File.exist?("specs")
  end

  def caveats
    <<~EOS
      Run this once — it installs the shell hook into your ~/.zshrc and
      activates completion in the current session (the daemon starts
      automatically):

        eval "$(nerv init zsh)"

      `nerv doctor` verifies the install.
      `nerv uninstall` removes every trace.
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

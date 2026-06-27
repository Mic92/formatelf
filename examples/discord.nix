# Prebuilt Electron application, the heaviest common autoPatchelfHook case.
# The tarball ships the main binary plus vendored shared objects (libffmpeg.so,
# the GL/Vulkan stack), so the build relies on every hook feature:
# buildInputs resolution, addAutoPatchelfSearchPath for the sibling
# libffmpeg.so, runtimeDependencies/appendRunpaths for dlopen-only libraries,
# and autoPatchelfIgnoreMissingDeps.
{
  lib,
  stdenv,
  fetchurl,
  autoFormatelfHook,
  # graphical / runtime stack
  alsa-lib,
  at-spi2-atk,
  at-spi2-core,
  atk,
  cairo,
  cups,
  dbus,
  expat,
  gdk-pixbuf,
  glib,
  gtk3,
  libGL,
  libdrm,
  libnotify,
  libpulseaudio,
  libuuid,
  libx11,
  libxcb,
  libxcomposite,
  libxdamage,
  libxext,
  libxfixes,
  libxkbcommon,
  libxrandr,
  mesa,
  nspr,
  nss,
  pango,
  systemd,
}:

stdenv.mkDerivation (finalAttrs: {
  pname = "discord";
  version = "0.0.75";

  src = fetchurl {
    url = "https://dl.discordapp.net/apps/linux/${finalAttrs.version}/discord-${finalAttrs.version}.tar.gz";
    hash = "sha256-mkqyc9Co8ineL6dcJKaRJD4TVG1RYij//OgzMh/tLDA=";
  };

  nativeBuildInputs = [ autoFormatelfHook ];

  buildInputs = [
    alsa-lib
    at-spi2-atk
    at-spi2-core
    atk
    cairo
    cups
    dbus
    expat
    gdk-pixbuf
    glib
    gtk3
    libdrm
    libnotify
    mesa # libgbm
    nspr
    nss
    pango
    stdenv.cc.cc # libstdc++ / libgcc_s
    libx11
    libxcomposite
    libxdamage
    libxext
    libxfixes
    libxrandr
    libxcb
    libxkbcommon
  ];

  # Pulled in via dlopen at runtime, so they must reach the RUNPATH even though
  # no DT_NEEDED entry points at them.
  runtimeDependencies = [
    (lib.getLib systemd) # libudev
    libGL
    libpulseaudio
    libuuid
  ];

  # These are looked up lazily and Discord copes when they are missing.
  autoPatchelfIgnoreMissingDeps = [
    "libappindicator3.so.1"
    "libdbusmenu-glib.so.4"
  ];

  installPhase = ''
    runHook preInstall

    mkdir -p $out/opt/Discord $out/bin
    cp -r . $out/opt/Discord
    ln -s $out/opt/Discord/Discord $out/bin/discord

    runHook postInstall
  '';

  # The main binary needs its vendored libffmpeg.so, which lives next to it
  # rather than in any buildInput; register that directory before patching.
  preFixup = ''
    addAutoPatchelfSearchPath $out/opt/Discord
  '';

  # Prove the patching actually took: ask the dynamic loader to resolve every
  # ELF's DT_NEEDED through the new RUNPATH (no display required) and fail on
  # any unresolved library.
  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck

    local failed=0
    while IFS= read -r f; do
      trace=$(ldd "$f" 2>/dev/null) || continue
      if grep -q 'not found' <<<"$trace"; then
        echo "unresolved dependencies in $f:" >&2
        grep 'not found' <<<"$trace" >&2
        failed=1
      fi
    done < <(find $out -type f \( -perm -u+x -o -name '*.so*' \))
    [ "$failed" -eq 0 ]

    runHook postInstallCheck
  '';

  meta = {
    description = "Prebuilt Discord client, patched with auto-formatelf";
    platforms = [ "x86_64-linux" ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
})

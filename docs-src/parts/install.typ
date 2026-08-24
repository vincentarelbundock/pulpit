
== macOS

With Homebrew:

```sh
brew install --cask vincentarelbundock/tap/pulpit
```

Either way the first launch needs the one-time approval described below;
Homebrew no longer offers a flag that skips it.

Or install it by hand:

+ Download the disk image from the releases page.
+ Open it and drag `Pulpit.app` onto Applications.
+ Launch Pulpit. macOS refuses this first launch.
+ Open System Settings, then Privacy & Security (called Security Settings in
  some versions), and click *Open Anyway* next to the message about Pulpit.
+ Launch Pulpit again. It opens, and the refusal does not come back, this
  version or any later one.

That refusal happens because the app is not registered with Apple, which costs
money, even for free open source projects.

One download covers both Apple Silicon and Intel Macs, running natively on
each. macOS 11 Big Sur or newer.

Install both media players too. This is strongly recommended, since without
them video and interactive slides fall back to their poster image:

```sh
brew install --cask google-chrome
brew install mpv
```

== Windows

With a package manager:

```sh
winget install VincentArelBundock.Pulpit
scoop install pulpit
```

Or install it by hand:

+ Download the installer from the releases page. A portable `.zip` is
  published beside it if you would rather not install anything.
+ Run the installer. It installs for your account only, so it never asks for
  an administrator password.
+ Windows may warn you that it does not recognise the publisher. Click *More
  info*, then *Run anyway*.

That warning happens because the builds are not signed, which costs money,
even for free open source projects.

Builds are 64-bit Intel only at present. Windows on ARM runs them anyway.

Edge is already installed, so video and interactive slides work as they are.
Adding mpv is still strongly recommended: it plays video better, and any mpv
build that ships `mpv-2.dll` on your `PATH` will do.

== Linux

Install the package for your distribution:

+ Download the appropriate package from the releases page.
+ Install it with your package manager.

```sh
sudo apt install ./pulpit_*_amd64.deb
sudo dnf install ./pulpit-*.x86_64.rpm
sudo pacman -U ./pulpit-*-x86_64.pkg.tar.zst
```

The package installs what Pulpit depends on alongside it, and suggests both
media players. Accept them, which is strongly recommended, or install them
yourself:

```sh
sudo apt install chromium mpv
```

Building from source works too, and the repository's `README` covers it.

== Nix

Nix is a cross-platform package manager that runs on Linux and macOS; its
#link("https://nixos.org/download/")[download page] covers installing it. If
you already have it, this runs Pulpit once, without installing anything:

```sh
nix run github:vincentarelbundock/pulpit -- path/to/deck.pdf
```

And this installs it for your user, so it stays:

```sh
nix profile add github:vincentarelbundock/pulpit
```

On Nix older than 2.30 the subcommand is `nix profile install`, which newer
versions still accept with a deprecation warning.

The packaged build starts with no setup at all: it already knows where to find
the libraries it loads while running. That matters most on NixOS, where those
libraries do not sit in the usual system-wide location. It also carries the
two media players itself, so video and interactive slides work immediately.

== Video and interactions

Pulpit can display video and interactive content (ex: HTML+JS) on top of PDF slideshows. For this, you need:

+ A Chromium-based browser (ex: Chrome, Chromium, Edge and Brave).
+ #link("https://mpv.io/")[The mpv video player.] 

When displaying interactive content, Pulpit never uses your own browser session: it starts the browser hidden, in a private profile per deck, so your extensions, cookies, logins and history are untouched.

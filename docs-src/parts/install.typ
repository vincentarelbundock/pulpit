
== macOS

With Homebrew:

```sh
brew install --cask vincentarelbundock/tap/pulpit
```

Or install it by hand:

+ Download the disk image from the Github releases page.
+ Open it and drag `Pulpit.app` onto Applications.

Either way, on first launch macOS will likely refuse to launch Pulpit. That refusal happens because the app is not registered with Apple, which costs money, even for free open source projects.

+ Open System Settings, then Privacy & Security (called Security Settings in
  some versions), and click *Open Anyway* next to the message about Pulpit.
+ Launch Pulpit again. It opens, and the refusal does not come back, this
  version or any later one.

You should probably install both media players too. This is strongly recommended, since without them video and interactive slides fall back to their poster image:

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

== DjVu

Pulpit reads DjVu books (`.djvu` and `.djv`) if
#link("https://djvu.sourceforge.net/")[djvulibre] is installed. Pulpit does
not ship a copy: DjVu is a capability of your machine rather than of the
build you downloaded, so the same Pulpit reads DjVu on a computer that has
the library and says so plainly on one that does not.

Install it for your platform:

```sh
# macOS
brew install djvulibre

# Debian, Ubuntu, Mint
sudo apt install libdjvulibre21

# Fedora, RHEL, CentOS
sudo dnf install djvulibre-libs

# Arch, Manjaro
sudo pacman -S djvulibre

# openSUSE
sudo zypper install libdjvulibre21

# Alpine
sudo apk add djvulibre-libs

# Nix, NixOS — add djvulibre to your environment, or just:
nix shell nixpkgs#djvulibre
```

On Windows there is no package to install: get the DjVuLibre binaries from
#link("https://djvu.sourceforge.net/")[the DjVuLibre site] and put
`libdjvulibre.dll` somewhere on your `PATH`, or beside `pulpit.exe`.

Nothing needs configuring afterwards. Pulpit looks for the library each time
it opens a DjVu, so it is found the next time you open a book — there is no
setting to change and no rebuild. Opening a DjVu without the library gives
you a message naming the format and repeating the install line for your
platform, rather than complaining that the file is damaged.

If the library is installed somewhere Pulpit's loader does not look, name it
directly:

```sh
PULPIT_DJVU_PATH=/path/to/lib pulpit book.djvu
```

That variable takes either the directory holding the library or the library
file itself.

DjVu books are read-only in Pulpit — no annotations, forms, text selection,
search or signing. The DjVu notes under #emph[Reading] explain why, and how to
convert a scan to PDF if you need to mark it up.

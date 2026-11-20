# BSDConqueror - Makefile (compatível com bmake/FreeBSD e GNU make)
#
# Instalação a partir do código-fonte:
#   make
#   make install        (como root, ou com DESTDIR para staging)
#
# Exemplos:
#   make PREFIX=/usr/local install
#   make DESTDIR=/tmp/pkg PREFIX=/usr/local install

PREFIX?=	/usr/local
BINDIR?=	$(PREFIX)/bin
DATADIR?=	$(PREFIX)/share
APPLICATIONSDIR?=$(DATADIR)/applications
ICONDIR_128?=	$(DATADIR)/icons/hicolor/128x128/apps
ICONDIR_SVG?=	$(DATADIR)/icons/hicolor/scalable/apps

CARGO?=		cargo
CARGO_FLAGS?=	--release
BIN=		target/release/bsdconqueror

.PHONY: all build release install uninstall clean distclean test check fmt

all: build

build release:
	$(CARGO) build $(CARGO_FLAGS)

$(BIN): build

install: $(BIN)
	install -d $(DESTDIR)$(BINDIR)
	install -m 0755 $(BIN) $(DESTDIR)$(BINDIR)/bsdconqueror
	install -d $(DESTDIR)$(APPLICATIONSDIR)
	install -m 0644 bsdconqueror.desktop $(DESTDIR)$(APPLICATIONSDIR)/bsdconqueror.desktop
	install -d $(DESTDIR)$(ICONDIR_128)
	install -m 0644 src/icons/app-128x128.png $(DESTDIR)$(ICONDIR_128)/bsdconqueror.png
	install -d $(DESTDIR)$(ICONDIR_SVG)
	install -m 0644 src/icons/svg/app.svg $(DESTDIR)$(ICONDIR_SVG)/bsdconqueror.svg

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/bsdconqueror
	rm -f $(DESTDIR)$(APPLICATIONSDIR)/bsdconqueror.desktop
	rm -f $(DESTDIR)$(ICONDIR_128)/bsdconqueror.png
	rm -f $(DESTDIR)$(ICONDIR_SVG)/bsdconqueror.svg

clean:
	$(CARGO) clean

distclean: clean

test:
	$(CARGO) test

check:
	$(CARGO) check

fmt:
	$(CARGO) fmt

PREFIX    ?= $(HOME)/.local
BINDIR    ?= $(PREFIX)/bin
SHAREDIR  ?= $(PREFIX)/share
BASHDIR   ?= $(SHAREDIR)/bash-completion/completions
FISHDIR   ?= $(SHAREDIR)/fish/vendor_completions.d
ZSHDIR    ?= $(SHAREDIR)/zsh/site-functions

CARGO     ?= cargo
INSTALL   ?= install

.PHONY: build release install uninstall clean

build:
	$(CARGO) build

release:
	$(CARGO) build --release

install: release
	$(INSTALL) -d $(BINDIR)
	$(INSTALL) -m 755 target/release/kubie $(BINDIR)/kubie
	$(INSTALL) -d $(BASHDIR)
	$(INSTALL) -m 644 completion/kubie.bash $(BASHDIR)/kubie
	$(INSTALL) -d $(FISHDIR)
	$(INSTALL) -m 644 completion/kubie.fish $(FISHDIR)/kubie.fish
	$(INSTALL) -d $(ZSHDIR)
	target/release/kubie generate-completion zsh > $(ZSHDIR)/_kubie

uninstall:
	rm -f $(BINDIR)/kubie
	rm -f $(BASHDIR)/kubie
	rm -f $(FISHDIR)/kubie.fish
	rm -f $(ZSHDIR)/_kubie

clean:
	$(CARGO) clean

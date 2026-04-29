PREFIX    ?= $(HOME)/.local
BINDIR    ?= $(PREFIX)/bin
SHAREDIR  ?= $(PREFIX)/share
BASHDIR   ?= $(SHAREDIR)/bash-completion/completions
FISHDIR   ?= $(SHAREDIR)/fish/vendor_completions.d
ZSHDIR    ?= $(SHAREDIR)/zsh/site-functions

TARGET    ?= x86_64-unknown-linux-musl
CARGO     ?= cargo
INSTALL   ?= install

.PHONY: build fast release install dist uninstall clean

build:
	$(CARGO) build --target $(TARGET)

fast:
	$(CARGO) build --profile fast --target $(TARGET)

release:
	$(CARGO) build --release --target $(TARGET)

install: fast
	$(INSTALL) -d $(BINDIR)
	$(INSTALL) -m 755 target/$(TARGET)/fast/kubie $(BINDIR)/kubie
	$(INSTALL) -d $(BASHDIR)
	$(INSTALL) -m 644 completion/kubie.bash $(BASHDIR)/kubie
	$(INSTALL) -d $(FISHDIR)
	$(INSTALL) -m 644 completion/kubie.fish $(FISHDIR)/kubie.fish
	$(INSTALL) -d $(ZSHDIR)
	$(BINDIR)/kubie generate-completion zsh > $(ZSHDIR)/_kubie

dist: release
	$(INSTALL) -d $(BINDIR)
	$(INSTALL) -m 755 target/$(TARGET)/release/kubie $(BINDIR)/kubie
	$(INSTALL) -d $(BASHDIR)
	$(INSTALL) -m 644 completion/kubie.bash $(BASHDIR)/kubie
	$(INSTALL) -d $(FISHDIR)
	$(INSTALL) -m 644 completion/kubie.fish $(FISHDIR)/kubie.fish
	$(INSTALL) -d $(ZSHDIR)
	$(BINDIR)/kubie generate-completion zsh > $(ZSHDIR)/_kubie

uninstall:
	rm -f $(BINDIR)/kubie
	rm -f $(BASHDIR)/kubie
	rm -f $(FISHDIR)/kubie.fish
	rm -f $(ZSHDIR)/_kubie

clean:
	$(CARGO) clean

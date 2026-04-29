PREFIX    ?= $(HOME)/.local
BINDIR    ?= $(PREFIX)/bin
SHAREDIR  ?= $(PREFIX)/share
BASHDIR   ?= $(SHAREDIR)/bash-completion/completions
FISHDIR   ?= $(SHAREDIR)/fish/vendor_completions.d
ZSHDIR    ?= $(SHAREDIR)/zsh/site-functions

TARGET    ?= x86_64-unknown-linux-musl
CARGO     ?= cargo
INSTALL   ?= install

.PHONY: build release install uninstall clean publish-tag

build:
	$(CARGO) build --target $(TARGET)

release:
	$(CARGO) build --release --target $(TARGET)

install: release
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

publish-tag:
	@VERSION=$$(date +%Y.%-m.%-d); \
	PATCH=0; \
	while git rev-parse "v$$VERSION$$([ $$PATCH -gt 0 ] && echo .$$PATCH)" >/dev/null 2>&1; do \
		PATCH=$$((PATCH + 1)); \
	done; \
	if [ $$PATCH -gt 0 ]; then VERSION="$$VERSION.$$PATCH"; fi; \
	sed -i "s/^version = \".*\"/version = \"$$VERSION\"/" Cargo.toml; \
	echo "v$$VERSION"; \
	git add Cargo.toml; \
	git commit -m "v$$VERSION"; \
	git tag "v$$VERSION"

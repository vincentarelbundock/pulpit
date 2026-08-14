# Example decks.
#
#   make            build every example PDF
#   make assets     regenerate media-assets/ from scratch
#   make verify     check the built PDF's media URIs and attachments
#   make clean      remove LaTeX by-products (keeps the PDFs)

LATEXMK ?= latexmk
PDFLATEX ?= pdflatex
PYTHON ?= python3
TYPST ?= typst

PDFS := combined.pdf mosaic.pdf

ASSETS := \
	media-assets/bouncing.gif \
	media-assets/bouncing-still.png \
	media-assets/clip.mp4 \
	media-assets/poster.png \
	media-assets/balls-poster.png \
	media-assets/bouncing-balls.html

.PHONY: all assets verify clean distclean

all: $(PDFS)

combined.pdf: combined.tex $(ASSETS)
	@if command -v $(LATEXMK) >/dev/null 2>&1; then \
		$(LATEXMK) -pdf -interaction=nonstopmode $<; \
	else \
		$(PDFLATEX) -interaction=nonstopmode $< && \
		$(PDFLATEX) -interaction=nonstopmode $<; \
	fi

mosaic.pdf: mosaic.typ $(ASSETS)
	$(TYPST) compile $< $@

# All six assets come out of one deterministic script, so they share a rule.
$(ASSETS): make-media-assets.sh bouncing-balls.html
	./make-media-assets.sh

assets:
	./make-media-assets.sh

verify: $(PDFS)
	$(PYTHON) verify-media-pdf.py combined.pdf
	$(PYTHON) verify-media-pdf.py mosaic.pdf

clean:
	rm -f *.aux *.log *.nav *.out *.snm *.toc *.fls *.fdb_latexmk *.vrb

distclean: clean
	rm -f $(PDFS)

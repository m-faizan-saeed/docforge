"""Writes two small vector PDFs for trying "remove white" with PDF Overlay:

  white-background.pdf  a page-sized white box with black art and text on it
  red-stripes.pdf       red stripes, to lay under the first one

    python3 inputs/make_white_bg.py inputs
"""
import sys
from pathlib import Path


def write_pdf(path, content):
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] "
        b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for i, body in enumerate(objects, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % i + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    out += b"".join(b"%010d 00000 n \n" % o for o in offsets)
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (
        len(objects) + 1,
        xref,
    )
    Path(path).write_bytes(out)


folder = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
write_pdf(
    folder / "white-background.pdf",
    b"1 g 0 0 300 200 re f\n"
    b"0 g 30 30 80 80 re f\n"
    b"0 G 4 w 150 40 m 270 160 l S\n"
    b"BT /F1 28 Tf 150 150 Td (TOP) Tj ET\n",
)
stripes = b"1 0 0 rg\n" + b"".join(b"%d 0 12 200 re f\n" % x for x in range(0, 300, 30))
write_pdf(folder / "red-stripes.pdf", stripes)

"""Synthesise a phone photo of a printed page: skewed, shaded, on a desk."""
import sys, random
import numpy as np
from PIL import Image, ImageDraw, ImageFont, ImageFilter

out = sys.argv[1]
random.seed(4)
W, H = 1700, 2200
page = Image.new("RGB", (W, H), (246, 243, 236))
d = ImageDraw.Draw(page)
try:
    big = ImageFont.truetype("/usr/share/fonts/noto/NotoSans-Regular.ttf", 90)
    body = ImageFont.truetype("/usr/share/fonts/noto/NotoSans-Regular.ttf", 38)
except OSError:
    big = body = ImageFont.load_default()
d.text((140, 150), "INVOICE  #2026-104", fill=(25, 25, 30), font=big)
y = 380
words = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt".split()
for i in range(32):
    d.text((140, y), " ".join(random.choice(words) for _ in range(random.randint(6, 9))), fill=(40, 40, 48), font=body)
    y += 52
d.ellipse((1150, 1650, 1500, 2000), outline=(200, 30, 40), width=12)
d.text((1215, 1790), "PAID", fill=(200, 30, 40), font=big)

PW, PH = 2400, 3000
quad = [(330, 260), (2080, 380), (2200, 2780), (210, 2650)]
def coeffs(src, dst):
    A, B = [], []
    for (x, y), (u, v) in zip(dst, src):
        A.append([x, y, 1, 0, 0, 0, -u * x, -u * y]); B.append(u)
        A.append([0, 0, 0, x, y, 1, -v * x, -v * y]); B.append(v)
    return np.linalg.solve(np.array(A, float), np.array(B, float)).tolist()
warped = page.transform((PW, PH), Image.Transform.PERSPECTIVE, coeffs([(0, 0), (W, 0), (W, H), (0, H)], quad), Image.Resampling.BICUBIC)
photo = Image.new("RGB", (PW, PH), (74, 58, 46))
mask = Image.new("L", (PW, PH), 0); ImageDraw.Draw(mask).polygon(quad, fill=255)
photo.paste(warped, (0, 0), mask)
shade = Image.linear_gradient("L").resize((PW, PH))
photo = Image.composite(photo, Image.eval(photo, lambda v: int(v * 0.7)), shade)
photo = photo.filter(ImageFilter.GaussianBlur(1.1))
photo.save(out, quality=90)

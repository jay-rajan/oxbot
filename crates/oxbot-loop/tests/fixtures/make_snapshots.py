#!/usr/bin/env python3
"""Writes the page snapshot fixtures in snapshots/ (example.com sites only).

The text imitates chrome-devtools-mcp 1.10.1: one `uid=` line per node, two
spaces per depth, `none` printed as `ignored`, attributes sorted by name with
the `focusable`/`expandable`/`disableable`/`selectable` companions, and the
noise real pages have (InlineTextBox, unnamed generic nodes, StaticText runs,
long names). Run: python3 -I make_snapshots.py
"""

import os

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "snapshots")
COMPANION = {"disabled": "disableable", "expanded": "expandable",
             "focused": "focusable", "selected": "selectable"}


class Page:
    def __init__(self, snap_id):
        self.snap_id = snap_id
        self.count = 0
        self.lines = []

    def node(self, depth, role, name=None, **attrs):
        uid = f"{self.snap_id}_{self.count}"
        self.count += 1
        parts = [f"uid={uid}"]
        if role:
            parts.append("ignored" if role == "none" else role)
        if name:
            parts.append(f'"{name}"')
        for key in sorted(attrs):
            val = attrs[key]
            if key in COMPANION and isinstance(val, bool):
                parts.append(COMPANION[key])
            if val is True:
                parts.append(key)
            elif isinstance(val, (str, int)) and not isinstance(val, bool):
                parts.append(f'{key}="{val}"')
        self.lines.append("  " * depth + " ".join(parts))
        return uid

    def text(self, depth, words, boxes=True):
        """StaticText with InlineTextBox children, like a verbose tree."""
        self.node(depth, "StaticText", words)
        if boxes:
            self.node(depth + 1, "InlineTextBox", words)


def write(name, head, page, tail=""):
    body = head + "## Latest page snapshot\n" + "\n".join(page.lines) + "\n" + tail
    with open(os.path.join(OUT, f"{name}.txt"), "w") as f:
        f.write(body)


def search():
    p = Page(3)
    url = "https://shop.example.com/search?q=lamp"
    p.node(0, "RootWebArea", "lamp - Shop", url=url)
    p.node(1, "banner")
    p.node(2, "link", "Shop home", url="https://shop.example.com/")
    p.node(3, "image", "Shop")
    p.node(2, "generic")
    p.node(3, "searchbox", "Search", focused=True, value="lamp")
    p.node(3, "button", "Search", focused=False)
    p.node(2, "link", "Cart (2 items)", url="https://shop.example.com/cart")
    p.node(1, "navigation", "Departments")
    p.node(2, "list")
    for dep in ["Home", "Kitchen", "Garden", "Office", "Lighting", "Toys",
                "Books", "Sports", "Tools", "Pets", "Music", "Deals"]:
        p.node(3, "listitem")
        p.node(4, "ListMarker", "•")
        p.node(4, "link", dep, url=f"https://shop.example.com/d/{dep.lower()}")
    p.node(1, "main")
    p.node(2, "heading", 'Results for "lamp"', level=1)
    p.text(3, 'Results for "lamp"')
    p.node(2, "generic")
    p.text(3, "1-300 of over 2,000 results")
    p.node(2, "form", "Filters")
    p.node(3, "group", "Price")
    for i, label in enumerate(["Under $25", "$25 to $50", "$50 to $100", "$100 & above"]):
        if i == 1:
            p.node(4, "checkbox", label, checked=True, focused=False)
        else:
            p.node(4, "checkbox", label, focused=False)
    p.node(3, "combobox", "Sort by", expanded=False, focused=False, haspopup="menu",
           value="Featured")
    p.node(3, "button", "Apply filters", focused=False)
    p.node(2, "list", "Search results")
    for i in range(1, 301):
        p.node(3, "listitem")
        p.node(4, "generic")
        p.node(5, "link", f"Desk lamp model {i}, adjustable arm, warm white LED",
               focused=False, url=f"https://shop.example.com/p/{i}?ref=search&q=lamp&pos={i}&sid=a1b2c3d4e5f6")
        p.text(6, f"Desk lamp model {i}, adjustable arm, warm white LED")
        p.node(4, "generic")
        p.text(5, f"${10 + (i * 7) % 90}.{(i * 13) % 100:02d}")
        p.node(4, "generic")
        p.text(5, f"{3 + (i % 3) * 0.5} out of 5 stars", boxes=False)
        p.text(5, f"({(i * 37) % 2000 + 1:,})", boxes=False)
        p.node(4, "button", "Add to cart", focused=False)
    p.node(2, "navigation", "Pagination")
    for n in range(1, 8):
        p.node(3, "link", f"Page {n}", url=f"https://shop.example.com/search?q=lamp&page={n}")
    p.node(3, "link", "Next", url="https://shop.example.com/search?q=lamp&page=2")
    p.node(1, "contentinfo")
    for label in ["About", "Help", "Privacy", "Terms"]:
        p.node(2, "link", label, url=f"https://shop.example.com/{label.lower()}")
    p.text(2, "© 2026 Shop")
    head = ("Successfully navigated to https://shop.example.com/search?q=lamp.\n"
            "## Pages\n1: lamp - Shop (https://shop.example.com/search?q=lamp) [selected]\n"
            "2: https://news.example.com/\n")
    write("search", head, p, "## Console messages\nmsgid=1 [log] page ready\n")


def product():
    p = Page(5)
    url = "https://shop.example.com/p/7"
    p.node(0, "RootWebArea", "Arc floor lamp - Shop", url=url)
    p.node(1, "banner")
    p.node(2, "link", "Shop home", url="https://shop.example.com/")
    p.node(2, "searchbox", "Search", focused=False)
    p.node(1, "navigation", "Breadcrumb")
    for label, path in [("Home", "/"), ("Lighting", "/d/lighting"), ("Floor lamps", "/d/lighting/floor")]:
        p.node(2, "link", label, url=f"https://shop.example.com{path}")
    p.node(1, "main")
    p.node(2, "heading", "Arc floor lamp, brushed steel, 190 cm", level=1)
    p.node(2, "image", "Arc floor lamp in a living room")
    p.node(2, "generic")
    p.text(3, "$129.00")
    p.text(3, "Free delivery by Friday")
    p.node(2, "paragraph")
    p.text(3, 'Rated "best value" by\nHome Weekly', boxes=False)
    p.node(2, "radiogroup", "Colour")
    for c, checked in [("Steel", True), ("Black", False), ("Brass", False)]:
        if checked:
            p.node(3, "radio", c, checked=True, focused=False)
        else:
            p.node(3, "radio", c, focused=False)
    p.node(2, "combobox", "Quantity", expanded=False, focused=False, haspopup="listbox", value="1")
    p.node(2, "button", "Add to cart", focused=False)
    p.node(2, "button", "Buy now", focused=False)
    p.node(2, "region", "Description")
    p.node(3, "heading", "Description", level=2)
    p.node(3, "paragraph")
    p.text(4, "A tall arc lamp that reaches over a sofa or a reading chair. The steel arm "
              "bends in one smooth curve, the marble base keeps it steady, and the shade "
              "turns so the light falls where you need it. The foot switch has three "
              "levels of brightness and the bulb is a warm white LED that lasts years.")
    p.node(3, "paragraph")
    p.text(4, "Ships in one box. Assembly takes about ten minutes, no tools needed.")
    p.node(2, "heading", "Specifications", level=2)
    p.node(2, "table", "Specifications")
    for k, v in [("Height", "190 cm"), ("Base", "Marble, 30 cm"), ("Bulb", "E27 LED, 9 W"),
                 ("Cable", "2 m, foot switch"), ("Weight", "14 kg")]:
        p.node(3, "row", f"{k} {v}")
        p.node(4, "rowheader", k)
        p.text(5, k)
        p.node(4, "cell", v)
        p.text(5, v)
    p.node(2, "heading", "Reviews", level=2)
    p.node(2, "generic")
    p.text(3, "4.6 out of 5 (212 reviews)")
    p.node(2, "list", "Reviews")
    reviews = ["Bright and steady, worth it.", "The base is heavy, as it should be.",
               "Took five minutes to set up.", "Light is warm, not yellow.",
               "Arm could be a bit longer.", "Great over my reading chair.",
               "Arrived with a scratch, support sent a new shade.", "Looks better than the photos."]
    for i, r in enumerate(reviews):
        p.node(3, "listitem")
        p.node(4, "generic")
        p.text(5, f"{5 - i % 2} out of 5 stars", boxes=False)
        p.node(4, "paragraph")
        p.text(5, r)
        p.node(4, "button", "Helpful", focused=False)
    p.node(2, "link", "See all 212 reviews", url="https://shop.example.com/p/7/reviews?sort=recent")
    p.node(1, "contentinfo")
    p.node(2, "link", "Help", url="https://help.example.com/shop")
    p.text(2, "© 2026 Shop")
    head = "## Pages\n1: Arc floor lamp - Shop (https://shop.example.com/p/7) [selected]\n"
    write("product", head, p)


def article():
    p = Page(8)
    url = "https://news.example.com/2026/10/ai-at-work"
    p.node(0, "RootWebArea", "How teams use AI at work - News", url=url)
    p.node(1, "banner")
    p.node(2, "link", "News", url="https://news.example.com/")
    p.node(2, "navigation", "Sections")
    for s in ["World", "Business", "Tech", "Science", "Culture", "Opinion"]:
        p.node(3, "link", s, url=f"https://news.example.com/{s.lower()}/")
    p.node(2, "button", "Subscribe", focused=False)
    p.node(1, "main")
    p.node(2, "article")
    p.node(3, "heading", "How teams use AI at work, one year in", level=1)
    p.node(3, "generic")
    p.text(4, "By Sam, Tech desk", boxes=False)
    p.node(4, "time")
    p.text(5, "Oct 30, 2026", boxes=False)
    paras = [
        "A year after the first wave of AI tools reached ordinary offices, the picture is "
        "less about replacing jobs and more about who learns to use them well. Teams that "
        "set clear rules for checking the output moved faster than teams that banned or "
        "ignored the tools.",
        "The pattern repeats across sectors. A small accounting firm cut its month-end "
        "close by two days, a design studio drafts three times as many concepts, and a "
        "city office answers routine letters in hours instead of weeks.",
        "Not every story is a success. Some teams shipped work they could not explain, "
        "and had to redo it when a client asked how a number was found.",
    ]
    p.node(3, "paragraph")
    p.text(4, paras[0])
    p.node(3, "paragraph")
    p.text(4, "The pattern repeats across sectors, says", boxes=False)
    p.node(4, "link", "a study from last spring", focused=False,
           url="https://research.example.org/papers/ai-office-2026#results")
    p.text(4, "." + paras[1][len("The pattern repeats across sectors."):], boxes=False)
    p.node(3, "heading", "What the fast teams did", level=2)
    p.node(3, "list")
    for item in ["They wrote down what a good answer looks like.",
                 "They checked every number against a source.",
                 "They kept a log of prompts that worked."]:
        p.node(4, "listitem")
        p.node(5, "ListMarker", "•")
        p.text(5, item, boxes=False)
    p.node(3, "figure")
    p.node(4, "image", "Chart: hours saved per week by team size")
    p.node(4, "Figcaption")
    p.text(5, "Hours saved per week, by team size. Source: News survey.", boxes=False)
    p.node(3, "heading", "Where it went wrong", level=2)
    p.node(3, "paragraph")
    p.text(4, paras[2])
    p.node(3, "blockquote")
    p.node(4, "paragraph")
    p.text(5, "“We saved time and then spent it again explaining ourselves,” said Ann, a team lead.")
    p.node(3, "heading", "What comes next", level=2)
    p.node(3, "paragraph")
    p.text(4, "Expect fewer new tools and more training. " * 6)
    p.node(1, "complementary", "Related")
    p.node(2, "heading", "Related", level=2)
    p.node(2, "list")
    for i, t in enumerate(["Five habits of teams that check their AI",
                           "The quiet rise of the AI-literate manager",
                           "Why small firms adopt faster"]):
        p.node(3, "listitem")
        p.node(4, "link", t, url=f"https://news.example.com/2026/10/related-{i + 1}")
    p.node(1, "contentinfo")
    for label in ["Contact", "Corrections", "Privacy"]:
        p.node(2, "link", label, url=f"https://news.example.com/{label.lower()}")
    head = ("## Pages\n1: How teams use AI at work - News "
            "(https://news.example.com/2026/10/ai-at-work) [selected]\n")
    write("article", head, p)


def table():
    p = Page(12)
    url = "https://data.example.com/reports/sales?year=2026"
    p.node(0, "RootWebArea", "Sales report 2026 - Data", url=url)
    p.node(1, "navigation", "Reports")
    for r in ["Sales", "Stock", "Returns"]:
        p.node(2, "link", r, url=f"https://data.example.com/reports/{r.lower()}")
    p.node(1, "main")
    p.node(2, "heading", "Sales by region, 2026", level=1)
    p.node(2, "button", "Download CSV", focused=False)
    p.node(2, "table", "Sales by region")
    p.node(3, "rowgroup")
    p.node(4, "row", "Region Month Units Revenue")
    for h in ["Region", "Month", "Units", "Revenue"]:
        p.node(5, "columnheader", h, focused=False)
        p.text(6, h, boxes=False)
    p.node(3, "rowgroup")
    regions = ["North", "South", "East", "West", "Central"]
    months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct"]
    for i in range(400):
        reg = regions[i % 5]
        mon = months[(i // 5) % 10]
        units = (i * 53) % 900 + 100
        rev = f"${units * 12:,}"
        p.node(4, "row", f"{reg} {mon} {units} {rev}")
        for v in [reg, mon, str(units), rev]:
            p.node(5, "cell", v)
            p.text(6, v, boxes=False)
    p.node(2, "navigation", "Table pages")
    p.node(3, "button", "Previous", disabled=True, focused=False)
    p.node(3, "button", "Next", focused=False)
    head = ("## Pages\n1: Sales report 2026 - Data "
            "(https://data.example.com/reports/sales?year=2026) [selected] isolatedContext=reports\n")
    write("table", head, p)


def feed():
    p = Page(20)
    url = "https://social.example.com/home"
    p.node(0, "RootWebArea", "Home - Social", url=url)
    p.node(1, "navigation", "Main menu")
    for s in ["Home", "Search", "Messages", "Profile"]:
        p.node(2, "link", s, url=f"https://social.example.com/{s.lower()}")
    p.node(1, "main")
    p.node(2, "heading", "Home", level=1)
    p.node(2, "textbox", "What's new?", focused=False, multiline=True)
    p.node(2, "button", "Post", disabled=True, focused=False)
    p.node(2, "feed", "Posts")
    people = ["Ann", "Sam", "Jay"]
    for i in range(120):
        who = people[i % 3]
        p.node(3, "article", f"Post by {who}")
        p.node(4, "link", who, url=f"https://social.example.com/@{who.lower()}")
        p.node(4, "link", f"{i + 1}h", url=f"https://social.example.com/@{who.lower()}/post/{1000 + i}")
        p.node(4, "generic")
        p.text(5, f"Day {i + 1} of learning to build with AI: today I tried a new way to check the "
                  f"output before I trust it. Note {i + 1}.", boxes=False)
        p.node(4, "group", "Actions")
        p.node(5, "button", f"Like, {i * 3 % 50} likes", pressed=False if i % 4 else True, focused=False)
        p.node(5, "button", "Reply", focused=False)
        p.node(5, "button", "Share", focused=False)
    p.node(1, "complementary", "Who to follow")
    for who in people:
        p.node(2, "link", f"Follow {who}", url=f"https://social.example.com/@{who.lower()}/follow")
    head = ("Page navigated to https://social.example.com/home.\n"
            "## Pages\n1: Home - Social (https://social.example.com/home) [selected]\n")
    write("feed", head, p)


def modal():
    p = Page(4)
    url = "https://shop.example.com/"
    p.node(0, "RootWebArea", "Shop", url=url)
    p.node(1, "banner")
    p.node(2, "link", "Shop home", url="https://shop.example.com/")
    p.node(2, "navigation", "Departments")
    for i in range(30):
        p.node(3, "link", f"Department {i + 1}", url=f"https://shop.example.com/d/{i + 1}")
    p.node(1, "main")
    p.node(2, "heading", "Autumn sale", level=1)
    p.node(2, "link", "Shop the sale", url="https://shop.example.com/sale?from=hero")
    p.node(2, "list", "Featured")
    for i in range(12):
        p.node(3, "listitem")
        p.node(4, "link", f"Featured item {i + 1}", url=f"https://shop.example.com/p/{100 + i}")
        p.node(4, "generic")
        p.text(5, f"${20 + i}.00", boxes=False)
    p.node(1, "alert")
    p.text(2, "Your cart was saved.", boxes=False)
    p.node(1, "dialog", "Sign in to continue", modal=True)
    p.node(2, "heading", "Sign in to continue", level=2)
    p.node(2, "textbox", "Email", focused=True, required=True)
    p.node(2, "textbox", "Password", focused=False, required=True)
    p.node(2, "checkbox", "Keep me signed in", focused=False)
    p.node(2, "button", "Sign in", focused=False)
    p.node(2, "link", "Forgot password?", url="https://shop.example.com/account/reset")
    p.node(2, "button", "Close", focused=False)
    head = "## Pages\n1: https://shop.example.com/ [selected]\n2: News (https://news.example.com/)\n"
    write("modal", head, p)


def form():
    p = Page(9)
    url = "https://jobs.example.com/apply/42"
    p.node(0, "RootWebArea", "Apply: Data engineer - Jobs", url=url)
    p.node(1, "banner")
    p.node(2, "link", "Jobs", url="https://jobs.example.com/")
    p.node(1, "main")
    p.node(2, "heading", "Apply for Data engineer", level=1)
    p.node(2, "alert")
    p.text(3, "Please fix 1 field: Email is not valid.", boxes=False)
    p.node(2, "form", "Application")
    p.node(3, "heading", "About you", level=2)
    p.node(3, "textbox", "Full name", focused=False, required=True, value="Jay")
    p.node(3, "textbox", "Email", focused=True, invalid="true", required=True, value="jay@")
    p.node(3, "textbox", "Phone", focused=False)
    p.node(3, "combobox", "Country", expanded=True, focused=False, haspopup="listbox", value="Canada")
    p.node(3, "listbox", "Country")
    for c in ["Canada", "Mexico", "United States"]:
        if c == "Canada":
            p.node(4, "option", c, focused=False, selected=True, value=c)
        else:
            p.node(4, "option", c, focused=False, selected=False, value=c)
    p.node(3, "heading", "Experience", level=2)
    p.node(3, "spinbutton", "Years of experience", focused=False, value="7", valuemax=50, valuemin=0)
    p.node(3, "slider", "Expected salary", focused=False, orientation="horizontal", value="120",
           valuemax=300, valuemin=40, valuetext="$120k")
    p.node(3, "radiogroup", "Work style")
    p.node(4, "radio", "On site", focused=False)
    p.node(4, "radio", "Hybrid", checked=True, focused=False)
    p.node(4, "radio", "Remote", focused=False)
    p.node(3, "group", "Skills")
    for s in ["Python", "SQL", "Spark", "Airflow", "dbt", "Kafka", "Rust", "Go"]:
        if s in ("SQL", "Airflow"):
            p.node(4, "checkbox", s, checked=True, focused=False)
        else:
            p.node(4, "checkbox", s, focused=False)
    p.node(3, "textbox", "Cover letter", focused=False, multiline=True,
           value="I build data pipelines and I like\nto know why the numbers hold.")
    p.node(3, "switch", "Email me about similar jobs", checked=True, focused=False)
    p.node(3, "button", "Submit application", disabled=True, focused=False)
    p.node(1, "contentinfo")
    p.node(2, "link", "Privacy", url="https://jobs.example.com/privacy")
    head = "## Pages\n1: Apply: Data engineer - Jobs (https://jobs.example.com/apply/42) [selected]\n"
    write("form", head, p)


if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    for make in (search, product, article, table, feed, modal, form):
        make()

// Routes the dispatchd domain (bound to this Worker via a Custom Domain,
// not the older Routes mechanism):
//
//   /                 -> redirects to https://get.graditya.com/dispatchd
//   /install.sh       -> redirects to https://get.graditya.com/dispatchd
//   /tos              -> cloudflare/tos.html
//   /privacy-policy   -> cloudflare/privacy-policy.html
//   anything else     -> 404
//
// The installer itself isn't a file in this repo - oxGrad/get generates
// it from functions/_shared/install-script.js (a shared template kept
// in sync with this repo's actual release contract: musl targets,
// SHA256SUMS, /usr/local/bin, the systemd restart prompt), and serves it
// at get.graditya.com/dispatchd. `curl -fsSL https://dispatchd.graditya.com | sudo sh`
// still works unchanged because curl's `-L` follows this redirect.
//
// /tos and /privacy-policy are still served straight from this repo's
// `main` branch (re-fetched from GitHub at most every 5 minutes), so
// editing either HTML file here is the only step needed to update what
// those two paths serve.
//
// Deploy: paste this into a new Worker in the Cloudflare dashboard
// (Workers & Pages -> Create), or `wrangler deploy` using the accompanying
// wrangler.toml. Then bind it to the domain under that Worker's
// Settings -> Domains & Routes -> Add -> Custom Domain.
//
// See ../docs/installing.md for the full step-by-step.

const REPO_RAW = "https://raw.githubusercontent.com/oxGrad/dispatchd/main";
const INSTALLER_URL = "https://get.graditya.com/dispatchd";

const REDIRECTS = new Set(["/", "/install.sh"]);

const FILE_ROUTES = {
  "/tos": { file: "/cloudflare/tos.html", type: "text/html; charset=utf-8" },
  "/privacy-policy": {
    file: "/cloudflare/privacy-policy.html",
    type: "text/html; charset=utf-8",
  },
};

export default {
  async fetch(request) {
    const { pathname } = new URL(request.url);
    const key = pathname.replace(/\/+$/, "") || "/";

    if (REDIRECTS.has(key)) {
      return Response.redirect(INSTALLER_URL, 302);
    }

    const route = FILE_ROUTES[key];
    if (!route) {
      return new Response("not found\n", { status: 404 });
    }

    const upstream = await fetch(REPO_RAW + route.file, {
      cf: { cacheTtl: 300 }, // re-fetch from GitHub at most every 5 minutes
    });

    if (!upstream.ok) {
      return new Response("failed to fetch content\n", { status: 502 });
    }

    return new Response(upstream.body, {
      headers: {
        "content-type": route.type,
        "cache-control": "public, max-age=300",
      },
    });
  },
};

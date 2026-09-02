// Routes the dispatchd domain (bound to this Worker via a Custom Domain,
// not the older Routes mechanism):
//
//   /                 -> install.sh  (so `curl https://dispatchd.graditya.com | sh` works)
//   /install.sh       -> install.sh
//   /tos              -> cloudflare/tos.html
//   /privacy-policy   -> cloudflare/privacy-policy.html
//   anything else     -> 404
//
// Every route is served straight from the repo's `main` branch (re-fetched
// from GitHub at most every 5 minutes), so editing a file in the repo is
// the only step needed to update what the domain serves.
//
// Deploy: paste this into a new Worker in the Cloudflare dashboard
// (Workers & Pages -> Create), or `wrangler deploy` using the accompanying
// wrangler.toml. Then bind it to the domain under that Worker's
// Settings -> Domains & Routes -> Add -> Custom Domain.
//
// See ../docs/installing.md for the full step-by-step.

const REPO_RAW = "https://raw.githubusercontent.com/oxGrad/dispatchd/main";

const ROUTES = {
  "/": { file: "/install.sh", type: "text/x-shellscript; charset=utf-8" },
  "/install.sh": { file: "/install.sh", type: "text/x-shellscript; charset=utf-8" },
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
    const route = ROUTES[key];

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

// Serves install.sh's raw content on every path of the domain this
// Worker is bound to (via a Custom Domain, not the older Routes
// mechanism) - so `curl https://dispatchd.graditya.com` (no path) gets
// the script directly, no redirect involved.
//
// Deploy: paste this into a new Worker in the Cloudflare dashboard
// (Workers & Pages -> Create), or `wrangler deploy` using the
// accompanying wrangler.toml. Then bind it to the domain under that
// Worker's Settings -> Domains & Routes -> Add -> Custom Domain.
//
// See ../docs/installing.md for the full step-by-step.

const SCRIPT_URL = "https://raw.githubusercontent.com/oxGrad/dispatchd/main/install.sh";

export default {
  async fetch() {
    const upstream = await fetch(SCRIPT_URL, {
      cf: { cacheTtl: 300 }, // re-fetch from GitHub at most every 5 minutes
    });

    if (!upstream.ok) {
      return new Response("failed to fetch install script\n", { status: 502 });
    }

    return new Response(upstream.body, {
      headers: {
        "content-type": "text/x-shellscript; charset=utf-8",
        "cache-control": "public, max-age=300",
      },
    });
  },
};

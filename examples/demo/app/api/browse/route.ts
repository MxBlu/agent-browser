import { NextRequest, NextResponse } from "next/server";
import * as ab from "@/lib/agent-browser";
import { ALLOWED_URLS } from "@/lib/constants";
import { minuteRateLimit, dailyRateLimit } from "@/lib/rate-limit";

export async function POST(req: NextRequest) {
  try {
    const ip =
      req.headers.get("x-forwarded-for")?.split(",")[0] ?? "anonymous";

    const minute = await minuteRateLimit.limit(ip);
    if (!minute.success) {
      return NextResponse.json(
        { error: "Too many requests. Please wait a moment before trying again." },
        { status: 429 },
      );
    }

    const daily = await dailyRateLimit.limit(ip);
    if (!daily.success) {
      return NextResponse.json(
        { error: "Daily limit reached. Please try again tomorrow." },
        { status: 429 },
      );
    }

    const body = await req.json();
    const url = body.url;

    if (!url) {
      return NextResponse.json({ error: "Provide a 'url'" }, { status: 400 });
    }

    if (!(ALLOWED_URLS as readonly string[]).includes(url)) {
      return NextResponse.json({ error: "URL not allowed" }, { status: 400 });
    }

    if (body.action === "screenshot") {
      const result = await ab.screenshotUrl(url, {
        fullPage: body.fullPage,
      });
      return NextResponse.json(result);
    }

    if (body.action === "snapshot") {
      const result = await ab.snapshotUrl(url, {
        selector: body.selector,
      });
      return NextResponse.json(result);
    }

    return NextResponse.json(
      { error: "Provide 'action' as 'screenshot' or 'snapshot'" },
      { status: 400 },
    );
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return NextResponse.json({ error: message }, { status: 502 });
  }
}

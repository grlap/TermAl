// Owns browser platform detection shared by keyboard shortcut classifiers.
// Does not own shortcut mapping or DOM event policy.

export function detectBrowserPlatform(): string {
  if (typeof navigator === "undefined") {
    return "";
  }

  const navigatorWithUserAgentData = navigator as Navigator & {
    userAgentData?: { platform?: string };
  };

  return (
    navigatorWithUserAgentData.userAgentData?.platform ??
    navigator.platform ??
    ""
  );
}

export function isApplePlatform(platform: string): boolean {
  return /mac|iphone|ipad|ipod/i.test(platform);
}

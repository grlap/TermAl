// Owns viewport/document visibility for decorative activity motion. One observer
// follows mounted indicators; it never polls or schedules animation frames.
const indicatorSelector = ".status-agent-badge, .activity-spinner, .composer-slash-status-spinner, .composer-slash-option-spinner, .message-activity-squares";

export function startActivityAnimationVisibility(root: HTMLElement): () => void {
  const doc = root.ownerDocument;
  const view = doc.defaultView;
  // Without visibility observations the CSS keeps a static working indicator.
  if (!view?.IntersectionObserver) return () => {};

  const indicators = new Set<Element>();
  const synchronize = (element: Element) => {
    if (doc.visibilityState !== "visible" || element.getAttribute("data-activity-visible") !== "true") return;
    // Sessions mount at different times. Keep their 30 Hz step boundaries
    // aligned instead of scheduling separate draws for each indicator.
    for (const animation of element.getAnimations({subtree: true})) {
      if (view.CSSAnimation && animation instanceof view.CSSAnimation &&
          (animation.animationName === "activity-step-spin" || animation.animationName === "transcript-activity-lift")) {
        animation.startTime = 0;
      }
    }
  };
  const intersections = new view.IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (indicators.has(entry.target)) {
        entry.target.setAttribute("data-activity-visible", String(entry.isIntersecting && entry.intersectionRatio > 0));
        synchronize(entry.target);
      }
    }
  });
  const visit = (node: Node, action: (element: Element) => void) => {
    if (!(node instanceof view.Element)) return;
    if (node.matches(indicatorSelector)) action(node);
    node.querySelectorAll(indicatorSelector).forEach(action);
  };
  const observe = (element: Element) => {
    if (indicators.has(element) || !root.contains(element)) return;
    indicators.add(element);
    element.setAttribute("data-activity-visible", "false");
    intersections.observe(element);
  };
  const forget = (element: Element) => {
    if (root.contains(element)) return; // A move within the app stays observed.
    indicators.delete(element);
    intersections.unobserve(element);
    element.removeAttribute("data-activity-visible");
  };
  const mutations = new view.MutationObserver((records) => {
    for (const record of records) {
      if (record.type === "attributes") {
        const element = record.target as Element;
        if (element.matches(indicatorSelector)) {
          observe(element);
          synchronize(element);
        }
        else if (indicators.delete(element)) {
          intersections.unobserve(element);
          element.removeAttribute("data-activity-visible");
        }
        continue;
      }
      record.removedNodes.forEach(node => visit(node, forget));
      record.addedNodes.forEach(node => visit(node, element => {
        observe(element);
        // Reinsertion can recreate CSS animations even for a tracked node.
        synchronize(element);
      }));
    }
  });
  mutations.observe(root, {childList: true, subtree: true, attributes: true, attributeFilter: ["class", "data-status", "data-work"]});
  visit(root, observe);
  const updateDocument = () => {
    root.setAttribute("data-activity-document-visible", String(doc.visibilityState === "visible"));
    indicators.forEach(synchronize);
  };
  updateDocument();
  doc.addEventListener("visibilitychange", updateDocument);
  const motion = view.matchMedia("(prefers-reduced-motion: reduce)");
  motion.addEventListener("change", updateDocument);
  return () => {
    mutations.disconnect();
    intersections.disconnect();
    doc.removeEventListener("visibilitychange", updateDocument);
    motion.removeEventListener("change", updateDocument);
    root.removeAttribute("data-activity-document-visible");
    indicators.forEach(element => element.removeAttribute("data-activity-visible"));
    indicators.clear();
  };
}

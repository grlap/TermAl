(() => {
  "use strict";

  const root = document.documentElement;
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  const supportsAnimation = typeof Element.prototype.animate === "function";
  let motionPaused = false;
  const pausedAnimations = new Set();

  const qs = (selector, scope = document) => scope.querySelector(selector);
  const qsa = (selector, scope = document) => [...scope.querySelectorAll(selector)];
  const listenMedia = (query, callback) => {
    if (typeof query.addEventListener === "function") query.addEventListener("change", callback);
    else if (typeof query.addListener === "function") query.addListener(callback);
  };

  function finishAnimation(animation) {
    animation.addEventListener("finish", () => {
      try {
        animation.commitStyles();
        animation.cancel();
      } catch {
        // Older engines may not implement commitStyles; the fill mode is enough.
      }
    }, { once: true });
    return animation;
  }

  function animateIn(element, keyframes, options) {
    if (!element || motionPaused || reducedMotion.matches || !supportsAnimation) return null;
    return finishAnimation(element.animate(keyframes, {
      duration: 700,
      easing: "cubic-bezier(.16, 1, .3, 1)",
      fill: "both",
      ...options,
    }));
  }

  function initHeader() {
    const header = qs("#site-header");
    const progress = qs("#scroll-progress-bar");
    const navLinks = qsa('.nav__links a[href^="#"]');
    const sections = navLinks
      .map((link) => qs(link.getAttribute("href")))
      .filter(Boolean);
    let ticking = false;

    function update() {
      const scrollTop = window.scrollY || document.documentElement.scrollTop;
      const scrollable = document.documentElement.scrollHeight - window.innerHeight;
      const amount = scrollable > 0 ? Math.min(1, Math.max(0, scrollTop / scrollable)) : 0;
      const marker = scrollTop + Math.min(window.innerHeight * 0.35, 280);
      let current = "";
      sections.forEach((section) => {
        if (section.offsetTop <= marker) current = section.id;
      });

      if (header) header.classList.toggle("is-scrolled", scrollTop > 18);
      if (progress) progress.style.transform = `scaleX(${amount})`;
      navLinks.forEach((link) => {
        const active = link.getAttribute("href") === `#${current}`;
        if (active) link.setAttribute("aria-current", "location");
        else link.removeAttribute("aria-current");
      });
      ticking = false;
    }

    function requestUpdate() {
      if (ticking) return;
      ticking = true;
      window.requestAnimationFrame(update);
    }

    if (document.readyState === "complete") requestUpdate();
    else window.addEventListener("load", requestUpdate, { once: true });
    window.addEventListener("scroll", requestUpdate, { passive: true });
    window.addEventListener("resize", requestUpdate, { passive: true });
  }

  function initMobileMenu() {
    const menu = qs("#mobile-menu");
    if (!menu) return;
    const summary = qs("summary", menu);
    const panel = qs(".mobile-menu__panel", menu);
    const pageMain = qs("main");
    const pageFooter = qs(".footer");
    const headerPeers = [qs(".brand", menu.parentElement), qs(".nav__links", menu.parentElement), qs(".nav__github", menu.parentElement)].filter(Boolean);
    const desktopBreakpoint = window.matchMedia("(min-width: 1021px)");

    function focusables() {
      return [summary, ...qsa("a[href]", panel)].filter((item) => !item.hasAttribute("disabled"));
    }

    function setBackgroundInert(inert) {
      if (pageMain) pageMain.inert = inert;
      if (pageFooter) pageFooter.inert = inert;
      headerPeers.forEach((element) => { element.inert = inert; });
    }

    function close({ restoreFocus = false } = {}) {
      if (!menu.open) return;
      menu.open = false;
      if (restoreFocus) summary.focus();
    }

    let focusAfterClose = null;
    menu.addEventListener("toggle", () => {
      const isOpen = menu.open;
      document.body.classList.toggle("menu-open", isOpen);
      setBackgroundInert(isOpen);
      if (isOpen) {
        window.requestAnimationFrame(() => qsa("a[href]", panel)[0]?.focus());
      } else if (focusAfterClose) {
        const target = focusAfterClose;
        focusAfterClose = null;
        // The panel is hidden and the background is no longer inert; land focus
        // on the destination rather than the now-hidden in-panel link.
        window.requestAnimationFrame(() => target.focus({ preventScroll: true }));
      }
    });

    menu.addEventListener("keydown", (event) => {
      if (!menu.open) return;
      if (event.key === "Escape") {
        event.preventDefault();
        close({ restoreFocus: true });
        return;
      }
      if (event.key !== "Tab") return;
      const items = focusables();
      const first = items[0];
      const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    });

    qsa('a[href^="#"]', panel).forEach((link) => link.addEventListener("click", () => {
      const href = link.getAttribute("href") || "";
      const target = href.length > 1 ? qs(href) : null;
      if (target) {
        // Make the destination section programmatically focusable so keyboard
        // users land there instead of on the collapsing panel link.
        if (!target.hasAttribute("tabindex")) target.setAttribute("tabindex", "-1");
        focusAfterClose = target;
        close();
      } else {
        close({ restoreFocus: true });
      }
    }));
    listenMedia(desktopBreakpoint, (event) => {
      if (event.matches) close();
    });
  }

  function initHeroIntro() {
    const sequence = [
      [qs(".hero__eyebrow"), 80],
      [qs(".hero__line:first-child > span"), 140],
      [qs(".hero__line--italic > span"), 220],
      [qs(".hero__lead"), 320],
      [qs(".hero__actions"), 400],
      [qs(".hero__plain-language"), 480],
      [qs(".control-room"), 260],
    ];
    sequence.forEach(([element, delay], index) => {
      const distance = index === sequence.length - 1 ? 22 : 28;
      animateIn(element, [
        { opacity: 0, transform: `translateY(${distance}px)` },
        { opacity: 1, transform: "translateY(0)" },
      ], { delay, duration: index === sequence.length - 1 ? 980 : 760 });
    });
  }

  function initScrollReveals() {
    const targets = qsa("[data-reveal]");
    if (!targets.length || reducedMotion.matches || !supportsAnimation || !("IntersectionObserver" in window)) return;

    const observer = new IntersectionObserver((entries) => {
      entries.forEach((entry) => {
        if (!entry.isIntersecting) return;
        const target = entry.target;
        observer.unobserve(target);
        const siblings = target.parentElement ? qsa(":scope > [data-reveal]", target.parentElement) : [];
        const siblingIndex = Math.max(0, siblings.indexOf(target));
        animateIn(target, [
          { opacity: 0, transform: "translateY(26px)" },
          { opacity: 1, transform: "translateY(0)" },
        ], { duration: 720, delay: Math.min(siblingIndex * 65, 260) });
      });
    }, { rootMargin: "0px 0px -10%", threshold: 0.08 });

    targets.forEach((target) => observer.observe(target));
  }

  function initControlRoom() {
    const room = qs("#control-room");
    const status = qs("#flow-status");
    const announcer = qs("#flow-announcer");
    const stepButtons = qsa("[data-flow-step]", room || document);
    const toggle = qs("#motion-toggle");
    if (!room || !toggle) return;

    const labels = [
      "The engineering brief is ready.",
      "Two delegated reviewers are working in parallel and reporting through durable mailboxes.",
      "Approval requested. Codex is waiting to run the workspace test suite.",
      "Approval complete. Delegated reviews returned, tests passed, and the diff is ready.",
    ];
    let state = reducedMotion.matches ? 3 : 0;
    let timer = 0;
    let roomVisible = true;
    let manualMode = reducedMotion.matches;

    function announce(message) {
      if (announcer) announcer.textContent = message;
    }

    function clearFlowTimer() {
      if (!timer) return;
      window.clearTimeout(timer);
      timer = 0;
    }

    function render(nextState, { speak = false } = {}) {
      state = Math.max(0, Math.min(3, Number(nextState)));
      room.dataset.flowState = String(state);
      stepButtons.forEach((button) => {
        button.setAttribute("aria-pressed", String(Number(button.dataset.flowStep) === state));
      });
      if (status) status.textContent = labels[state];
      if (speak) announce(labels[state]);
    }

    function scheduleFlow(delay = 1600) {
      clearFlowTimer();
      if (manualMode || motionPaused || reducedMotion.matches || !roomVisible || document.hidden || state >= 2) return;
      timer = window.setTimeout(() => {
        timer = 0;
        render(state + 1, { speak: state + 1 === 2 });
        scheduleFlow(state === 1 ? 1050 : 900);
      }, delay);
    }

    function pauseAllMotion() {
      motionPaused = true;
      root.classList.add("motion-paused");
      pausedAnimations.clear();
      document.getAnimations().forEach((animation) => {
        if (animation.playState !== "running") return;
        pausedAnimations.add(animation);
        animation.pause();
      });
      clearFlowTimer();
      toggle.setAttribute("aria-pressed", "true");
      qs("span", toggle).textContent = "Play motion";
    }

    function playAllMotion() {
      motionPaused = false;
      root.classList.remove("motion-paused");
      pausedAnimations.forEach((animation) => {
        try {
          if (animation.playState === "paused") animation.play();
        } catch {
          // An animation can disappear when its element leaves the document.
        }
      });
      pausedAnimations.clear();
      toggle.setAttribute("aria-pressed", "false");
      qs("span", toggle).textContent = "Pause motion";
      scheduleFlow(900);
    }

    render(state);
    if (reducedMotion.matches) {
      toggle.disabled = true;
      toggle.setAttribute("aria-pressed", "true");
      qs("span", toggle).textContent = "Motion reduced";
    } else {
      toggle.addEventListener("click", () => {
        if (motionPaused) playAllMotion();
        else pauseAllMotion();
      });
      scheduleFlow(900);
    }

    stepButtons.forEach((button) => {
      button.addEventListener("click", () => {
        manualMode = true;
        clearFlowTimer();
        render(button.dataset.flowStep, { speak: true });
      });
    });

    if ("IntersectionObserver" in window) {
      const observer = new IntersectionObserver(([entry]) => {
        roomVisible = Boolean(entry?.isIntersecting);
        if (roomVisible) scheduleFlow(700);
        else clearFlowTimer();
      }, { threshold: 0.1 });
      observer.observe(room);
    }

    document.addEventListener("visibilitychange", () => {
      if (document.hidden) clearFlowTimer();
      else scheduleFlow(700);
    });

    listenMedia(reducedMotion, (event) => {
      clearFlowTimer();
      if (event.matches) {
        manualMode = true;
        motionPaused = false;
        root.classList.remove("motion-paused");
        pausedAnimations.clear();
        document.getAnimations().forEach((animation) => {
          const iterations = animation.effect?.getTiming?.().iterations;
          if (Number.isFinite(iterations)) {
            try { animation.finish(); } catch { /* Animation is no longer active. */ }
          }
        });
        render(3);
        toggle.disabled = true;
        toggle.setAttribute("aria-pressed", "true");
        qs("span", toggle).textContent = "Motion reduced";
      } else {
        manualMode = false;
        motionPaused = false;
        root.classList.remove("motion-paused");
        pausedAnimations.forEach((animation) => {
          try { if (animation.playState === "paused") animation.play(); } catch { /* Element removed. */ }
        });
        pausedAnimations.clear();
        toggle.disabled = false;
        toggle.setAttribute("aria-pressed", "false");
        qs("span", toggle).textContent = "Pause motion";
        render(0);
        scheduleFlow(900);
      }
    });
  }

  function initThemePreview() {
    const demo = qs("#theme-demo");
    if (!demo) return;
    const choices = qsa("[data-theme-choice]", demo);
    const modeChoices = qsa("[data-theme-mode-choice]", demo);
    const pairCards = qsa("[data-theme-pair]", demo);
    const status = qs("#theme-demo-status span", demo);
    const themeState = window.TermalThemeDemoState;
    // The catalog remains a complete static no-JS presentation if its small
    // controller asset fails independently. Do not disable unrelated page
    // enhancements because this optional demo could not initialize.
    if (!themeState) return;
    const systemColorScheme = window.matchMedia("(prefers-color-scheme: dark)");
    const selected = {
      light: choices.find((choice) => choice.dataset.themeKind === "light" && choice.getAttribute("aria-pressed") === "true"),
      dark: choices.find((choice) => choice.dataset.themeKind === "dark" && choice.getAttribute("aria-pressed") === "true"),
    };
    let mode = modeChoices.find((choice) => choice.getAttribute("aria-pressed") === "true")?.dataset.themeModeChoice || "auto";
    let themeModeController;

    function themeName(choice) {
      return qs("b", choice)?.textContent?.trim() || choice.dataset.themeChoice;
    }

    function swatches(choice) {
      const styles = getComputedStyle(choice);
      return {
        background: styles.getPropertyValue("--swatch-bg").trim(),
        accent: styles.getPropertyValue("--swatch-a").trim(),
        detail: styles.getPropertyValue("--swatch-b").trim(),
      };
    }

    function updatePair(choice) {
      const kind = choice.dataset.themeKind;
      const pair = pairCards.find((card) => card.dataset.themePair === kind);
      if (!pair) return;
      const palette = swatches(choice);
      const preview = qs("[data-theme-pair-preview]", pair);
      qs("[data-theme-pair-name]", pair).textContent = themeName(choice);
      preview.style.setProperty("--swatch-bg", palette.background);
      preview.style.setProperty("--swatch-a", palette.accent);
      preview.style.setProperty("--swatch-b", palette.detail);
    }

    function render(choice) {
      if (!choice) return;
      const palette = swatches(choice);
      const name = themeName(choice);
      const activeKind = choice.dataset.themeKind;
      demo.dataset.previewTheme = choice.dataset.themeChoice;
      demo.dataset.previewKind = activeKind;
      demo.style.setProperty("--theme-bg", palette.background);
      demo.style.setProperty("--theme-accent", palette.accent);
      demo.style.setProperty("--theme-detail", palette.detail);

      choices.forEach((candidate) => candidate.classList.toggle("is-previewing", candidate === choice));
      pairCards.forEach((pair) => {
        const isActive = pair.dataset.themePair === activeKind;
        pair.classList.toggle("is-active", isActive);
        qs("[data-theme-pair-active]", pair).hidden = !isActive;
      });
      if (status) status.textContent = `${mode === "auto" ? "Auto preview" : mode === "light" ? "Light" : "Dark"} · ${name} active`;
    }

    choices.forEach((button) => {
      button.addEventListener("click", () => {
        const kind = button.dataset.themeKind;
        selected[kind] = button;
        choices.forEach((choice) => {
          const isSelected = choice === selected[choice.dataset.themeKind];
          choice.setAttribute("aria-pressed", String(isSelected));
        });
        updatePair(button);
        render(selected[themeModeController.getPreviewKind()]);
      });
      button.disabled = false;
    });

    themeModeController = themeState.createThemeModeController({
      initialMode: mode,
      mediaQuery: systemColorScheme,
      onPreviewKindChange: (nextKind) => render(selected[nextKind]),
    });

    modeChoices.forEach((button) => {
      button.addEventListener("click", () => {
        mode = button.dataset.themeModeChoice;
        modeChoices.forEach((choice) => choice.setAttribute("aria-pressed", String(choice === button)));
        themeModeController.setMode(mode);
      });
      button.disabled = false;
    });

    render(selected[themeModeController.getPreviewKind()]);
  }

  try {
    initHeader();
    initMobileMenu();
    initHeroIntro();
    initScrollReveals();
    initControlRoom();
    initThemePreview();
    root.classList.replace("no-js", "js");
    window.__termalReady = true;
  } catch (error) {
    console.error("TermAl website enhancement failed; keeping the static experience.", error);
  }
})();

export type RuntimeAction = "pause" | "resume" | "stop";

export function RuntimeActionButton({
  action,
  ariaLabel,
  title,
  classNamePrefix,
  isPending = false,
  disabled = false,
  onClick,
}: {
  action: RuntimeAction;
  ariaLabel: string;
  title: string;
  classNamePrefix: string;
  isPending?: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  const className =
    action === "stop"
      ? `ghost-button ${classNamePrefix} ${classNamePrefix}-stop`
      : `ghost-button ${classNamePrefix}`;

  return (
    <button
      className={className}
      type="button"
      aria-label={ariaLabel}
      title={title}
      aria-busy={isPending ? true : undefined}
      onClick={onClick}
      disabled={disabled}
    >
      {isPending ? (
        <span
          className={`activity-spinner ${classNamePrefix}-spinner`}
          aria-hidden="true"
        />
      ) : (
        <RuntimeActionIcon
          action={action}
          classNamePrefix={`${classNamePrefix}-icon`}
        />
      )}
    </button>
  );
}

export function RuntimeActionIcon({
  action,
  classNamePrefix,
}: {
  action: RuntimeAction;
  classNamePrefix: string;
}) {
  if (action === "pause") {
    return (
      <svg
        className={classNamePrefix}
        viewBox="0 0 16 16"
        focusable="false"
        aria-hidden="true"
      >
        <path
          d="M5.5 4.25v7.5M10.5 4.25v7.5"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeWidth="1.7"
        />
      </svg>
    );
  }

  if (action === "resume") {
    return (
      <svg
        className={classNamePrefix}
        viewBox="0 0 16 16"
        focusable="false"
        aria-hidden="true"
      >
        <path d="M5.35 4.35v7.3L11.6 8 5.35 4.35Z" fill="currentColor" />
      </svg>
    );
  }

  return (
    <svg
      className={`${classNamePrefix} ${classNamePrefix}-stop`}
      viewBox="0 0 16 16"
      focusable="false"
      aria-hidden="true"
    >
      <rect x="4.35" y="4.35" width="7.3" height="7.3" rx="1.2" fill="currentColor" />
    </svg>
  );
}

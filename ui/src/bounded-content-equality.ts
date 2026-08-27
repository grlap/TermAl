// Owns: bounded, order-independent structural equality for JSON-like UI data.
// Does not own: transcript transition policy or message-specific signatures.

type BoundedContentEqualityOptions = {
  maxDepth?: number;
  maxNodes?: number;
  maxStringCharacters?: number;
};

const DEFAULT_MAX_DEPTH = 32;
const DEFAULT_MAX_NODES = 4_096;
const DEFAULT_MAX_STRING_CHARACTERS = 262_144;

export function valuesHaveSameBoundedContent(
  left: unknown,
  right: unknown,
  options: BoundedContentEqualityOptions = {},
): boolean {
  const maxDepth = options.maxDepth ?? DEFAULT_MAX_DEPTH;
  let remainingNodes = options.maxNodes ?? DEFAULT_MAX_NODES;
  let remainingStringCharacters =
    options.maxStringCharacters ?? DEFAULT_MAX_STRING_CHARACTERS;
  const comparedPairs = new WeakMap<object, WeakSet<object>>();

  const consumeNodes = (count: number) => {
    remainingNodes -= count;
    return remainingNodes >= 0;
  };

  const compare = (leftValue: unknown, rightValue: unknown, depth: number): boolean => {
    if (typeof leftValue === "string" || typeof rightValue === "string") {
      if (
        typeof leftValue !== "string" ||
        typeof rightValue !== "string" ||
        leftValue.length !== rightValue.length ||
        leftValue.length > remainingStringCharacters
      ) {
        return false;
      }
      remainingStringCharacters -= leftValue.length;
      return leftValue === rightValue;
    }
    if (Object.is(leftValue, rightValue)) {
      return true;
    }
    if (depth >= maxDepth) {
      return false;
    }
    if (Array.isArray(leftValue) || Array.isArray(rightValue)) {
      if (
        !Array.isArray(leftValue) ||
        !Array.isArray(rightValue) ||
        leftValue.length !== rightValue.length ||
        !consumeNodes(1 + leftValue.length)
      ) {
        return false;
      }
      return leftValue.every((value, index) =>
        compare(value, rightValue[index], depth + 1),
      );
    }
    if (
      leftValue === null ||
      rightValue === null ||
      typeof leftValue !== "object" ||
      typeof rightValue !== "object"
    ) {
      return false;
    }

    const leftRecord = leftValue as Record<string, unknown>;
    const rightRecord = rightValue as Record<string, unknown>;
    const leftKeys = Object.keys(leftRecord);
    const rightKeys = Object.keys(rightRecord);
    if (
      leftKeys.length !== rightKeys.length ||
      !consumeNodes(1 + leftKeys.length)
    ) {
      return false;
    }

    const priorRightValues = comparedPairs.get(leftValue);
    if (priorRightValues?.has(rightValue)) {
      return true;
    }
    if (priorRightValues) {
      priorRightValues.add(rightValue);
    } else {
      comparedPairs.set(leftValue, new WeakSet([rightValue]));
    }

    return leftKeys.every(
      (key) =>
        Object.prototype.hasOwnProperty.call(rightRecord, key) &&
        compare(leftRecord[key], rightRecord[key], depth + 1),
    );
  };

  return compare(left, right, 0);
}

import { useRef } from "react";

/**
 * Retains the last non-null and non-undefined value so dialog content remains
 * visible during its closing animation.
 *
 * @param value Current value.
 * @returns The current value when valid, or the last valid value.
 */
export function useLastValidValue<T>(value: T | null | undefined): T | null {
  const ref = useRef<T | null>(null);

  // Update the ref synchronously during rendering rather than in an effect.
  if (value != null) {
    ref.current = value;
  }

  // Return the current value or the last valid value.
  return value ?? ref.current;
}

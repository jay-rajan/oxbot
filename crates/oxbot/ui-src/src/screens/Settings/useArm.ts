// Confirm in place: the first click arms a button ("Yes, sign out"), the
// second does it; left alone, it disarms after 5 s.
import { useCallback, useEffect, useRef, useState } from "react";

export function useArm(ms = 5000): {
  armed: string | null;
  /** True when `key` was armed (do it now); else arms it and returns false. */
  fire: (key: string) => boolean;
} {
  const [armed, setArmed] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current);
  }, []);
  const fire = useCallback(
    (key: string) => {
      if (timer.current) clearTimeout(timer.current);
      timer.current = null;
      if (armed === key) {
        setArmed(null);
        return true;
      }
      setArmed(key);
      timer.current = setTimeout(() => setArmed(null), ms);
      return false;
    },
    [armed, ms],
  );
  return { armed, fire };
}

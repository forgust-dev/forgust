import { useEffect, useRef } from "react";
import { useLocation, useNavigationType } from "react-router-dom";

/**
 * Back to the top when the section of a page changes (a settings page, a profile or admin page),
 * as a new page would. Not for an #anchor, which brings its own place, nor for back and forward,
 * where the browser's place is the right one.
 */
export function useTopOnSwitch(section: string) {
  const { hash } = useLocation();
  const how = useNavigationType();
  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    if (!hash && how !== "POP") window.scrollTo(0, 0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [section]);
}

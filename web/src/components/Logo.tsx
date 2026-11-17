import light from "../assets/brand/logo-light.png";
import dark from "../assets/brand/logo-dark.png";

/** The Forgust mark in both colorways; styles.css shows the one that suits the theme. */
export function Logo({ size, className }: { size: number; className?: string }) {
  const extra = className ? ` ${className}` : "";
  return (
    <>
      <img src={light} width={size} height={size} alt="" className={`logo logo-light${extra}`} />
      <img src={dark} width={size} height={size} alt="" className={`logo logo-dark${extra}`} />
    </>
  );
}

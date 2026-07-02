/** Zed-style Chevron 指示器 */
export function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      className={`flex-shrink-0 text-nexus-muted transition-transform ${open ? 'rotate-90' : ''}`}
      width="10" height="10" viewBox="0 0 10 10" fill="none"
      stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round"
    >
      <polyline points="3,1 7,5 3,9" />
    </svg>
  );
}

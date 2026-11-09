import { bodyUrl } from '../../lib/game';

export function Loading({ text = 'Brewing your board…' }: { text?: string }) {
  return (
    <div className="center-screen" role="status" aria-live="polite">
      <div className="loading">
        <img src={bodyUrl('boba')} alt="" />
        <p>{text}</p>
      </div>
    </div>
  );
}

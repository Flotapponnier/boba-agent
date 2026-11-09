import { useCallback, useEffect, useState } from 'react';
import { api } from './api/client';
import { connection } from './api/connection';
import type { AuthStatus } from './api/types';
import { AuthScreen, inviteToken } from './components/auth/AuthScreen';
import { Game } from './components/Game';
import { Sky } from './components/Sky';
import { Loading } from './components/ui/Loading';
import { Toasts } from './components/ui/Toasts';
import { useTheme } from './hooks/useTheme';
import { useBoard } from './store/store';
import { errorText } from './store/actions';

export function App() {
  const [auth, setAuth] = useState<AuthStatus | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const me = useBoard((s) => s.me);
  const theme = useTheme();

  const refresh = useCallback(async () => {
    try {
      setErr(null);
      const a = await api.authStatus();
      setAuth(a);
      useBoard.getState().setMe(a.me);
    } catch (e) {
      setErr(errorText(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const loggedIn = !!me && !inviteToken();
  useEffect(() => {
    if (!loggedIn) return;
    connection.start();
    return () => connection.stop();
  }, [loggedIn]);

  return (
    <div className={`app-root theme-${theme}`}>
      <Sky theme={theme} />
      {err ? (
        <div className="center-screen">
          <div className="wood card-sm">
            <div className="sheet">
              <h2 className="pixel-title">Can't reach Boba</h2>
              <p>{err}</p>
              <button className="btn gold" onClick={() => void refresh()}>
                Try again
              </button>
            </div>
          </div>
        </div>
      ) : !auth ? (
        <Loading />
      ) : !loggedIn ? (
        <AuthScreen setupDone={auth.setup_done} onDone={refresh} />
      ) : (
        <Game onLoggedOut={refresh} />
      )}
      <Toasts />
    </div>
  );
}

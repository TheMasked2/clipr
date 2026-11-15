export type Route = 'home' | 'gallery' | 'settings';
const routes: Route[] = ['home', 'gallery', 'settings'];

function parse(): Route {
  const h = location.hash.replace(/^#\/?/, '');
  return (routes as string[]).includes(h) ? (h as Route) : 'home';
}

export const router = $state<{ route: Route }>({ route: parse() });

window.addEventListener('hashchange', () => (router.route = parse()));

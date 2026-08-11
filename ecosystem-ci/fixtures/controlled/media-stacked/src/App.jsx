import styles from './App.module.css';

export default function App() {
  return <main>
    <section className={styles.panel} data-probe="panel" data-identity="panel">Panel</section>
    <aside className={styles.alert} data-probe="alert" data-identity="alert">Alert</aside>
    <button data-identity="ready">Ready</button>
  </main>;
}

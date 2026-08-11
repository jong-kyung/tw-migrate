import styles from './App.module.css';

export default function App() {
  return <main>
    <section className={styles.panel} data-probe="panel" data-identity="panel">Panel</section>
    <button data-identity="ready">Ready</button>
  </main>;
}

import styles from './App.module.less';

export default function App() {
  return <main>
    <article className={styles.card} data-probe="card" data-identity="card">Card</article>
    <button className={styles.action} data-identity="action">Toggle details</button>
    <section className={styles.responsive} data-probe="responsive-layout" data-identity="responsive">Responsive</section>
  </main>;
}

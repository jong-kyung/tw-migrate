import styles from './Card.module.css';

export default function Card() {
  return <article className={styles.card} data-probe="card" data-identity="card">Card</article>;
}

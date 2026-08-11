import './tailwind.css';
import styles from './Card.module.css';

export default function Card() {
  return <div className={styles.card} data-probe="card" data-identity="card">Card</div>;
}

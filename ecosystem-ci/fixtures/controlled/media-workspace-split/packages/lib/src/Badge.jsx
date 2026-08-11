import './tailwind.css';
import styles from './Badge.module.css';

export default function Badge() {
  return <div className={styles.badge} data-probe="badge" data-identity="badge">Badge</div>;
}
